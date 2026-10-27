# Giảm allocation khi kiểm tra phi một giá trị

Ngày 28/10/2026, UTC+7. Baseline `d3e64a2` (code; docs đến `e649fe8`).
Binary candidate SHA256 `f1f019b6564b9447774b622ffac023155fcef3c23bb7b4d3acfe13adf5b5b094`.

Trong shared `remove_unnecessary_params`, mượn các argument slices thay vì
copy một Vec references cho từng incoming edge. Kiểm tra đầu vào có cùng một
raw local bằng iterator thay vì dựng FxHashSet cho mỗi phi. Giữ thứ tự block,
parameter, dependency graph và local-map mutations. Chỉ loại self-edge của
captured cell theo proof cũ; hai raw aliases vẫn khác nhau dù local map dẫn
chúng tới cùng một local. Giữ validation/failure boundary cho incoming schemas
không khớp và mọi source-binding/capture guard. Không đổi API, proof budget hoặc
chính sách output; production code giảm 7 dòng.

Test mới đối chiếu independent eager/set reference trên 30 tổ hợp captured/
ordinary, 1/2/17 edges, duplicate/distinct/raw aliases/all-self/mixed-self inputs.
Giữ đối chiếu 128 CFGs, malformed schemas, SSA identities, map iteration order,
origins và captures đã có. Tests tập trung: 16 passed, 1 ignored. Workspace:
1.698 passed, 0 failed, 2 ignored; hai manual scaling probes vẫn ignored.

## Đo ordinary release

513 public inputs, same Rust/flags, CPU affinity theo 1/4 workers, artifact
cache và instrumentation tắt, output directories ấm. Xen kẽ hai labels mỗi
vòng; không build, CI hoặc profile đồng thời. Mọi sample xuất đủ 513 files và
output byte-identical với baseline.

| Lượt | Samples mỗi label | Median 1 worker trước → sau, s | Median 4 workers trước → sau, s |
|---|---:|---:|---:|
| Thăm dò | 7 | 0.707741 → 0.758580 (+7,2%) | 0.209830 → 0.219107 (+4,4%) |
| Xác nhận | 51 | 0.780020 → 0.764818 (-1,9%) | 0.214041 → 0.211305 (-1,3%) |
| Lặp độc lập | 51 | 0.750441 → 0.738390 (-1,6%) | 0.221571 → 0.220446 (-0,5%) |

Paired ratios của hai lượt dài là 0,97936 / 0,99285 và 0,97531 / 0,99143
ở 1/4 workers; số cặp nhanh hơn là 31/51 / 32/51 và 28/51 / 36/51.
Không bỏ lượt thăm dò trái chiều. Gộp 109 cặp để chẩn đoán: paired medians
0,98631 / 0,99335, thắng 60/109 / 70/109. Bootstrap paired-median 95%
percentile intervals (10.000 resamples, seed 27) là [0,97058; 1,00657] và
[0,98208; 0,99751]. Khoảng một worker vẫn chứa 1: chưa xác nhận speedup một
luồng. Các lần đo cùng corpus/cloud không phải những workload độc lập.
RSS median gần như không đổi. G1 vẫn mở.

Evidence: `/workspace/work/v27/phi-singleton-{tests,workspace,build}.log`,
`phi-singleton-r{7,51}-t{1,4}.json`,
`phi-singleton-replica-r51-t{1,4}.json`,
`phi-singleton-paired-analysis.json`, candidate patch và runners tương ứng.
Không có private corpus/Windows timing claim.

## Full audit và review output

Fixture and Luau audit đủ 33 bước đạt (34 results gồm lượt public baseline).
Semantic 801/801, deep review 469/469, public 513/513. So sánh bytes trực tiếp
với `d3e64a2`: toàn bộ 801 semantic, 513 public và 7 residual outputs giống
hệt; giữ 894/72/38 rebuilt-call annotations. Oracle và output-size baselines
đạt, không sửa baseline. Metadata/capture/producer, runtime, cache, Wasm và
compiler witnesses đều chạy qua workflow thật.

Giữ thay đổi nhỏ hơn này sau các kiểm chứng trên và kết quả bốn workers thuận
lợi qua hai lượt dài; không tuyên bố đã xác nhận speedup một worker hoặc đã
đạt G1. Lượt 135.000 profiles trước đó thuộc binary `8c5d3feb...` của
`d3e64a2`, không gán kết quả ấy cho candidate này. Các fuzz gates của CI đã
chạy trên candidate; không có private dumps hoặc original review reproducers.

Evidence bổ sung: `audit-25/results.json`, các log `audit-25-*.log`,
`phi-singleton-{semantic,public,residual}-comparison.json` và các empty patches.
Metadata JSON/cache lớn được gzip lossless, xác minh SHA256 trước khi thay raw;
source outputs và logs giữ nguyên. Cleanup dùng Python sau preview exact paths,
chỉ dọn các Cargo debug test executables có thể build lại; giữ baseline binaries
và evidence. Goal V2.7, G1 và các case thiếu đặc tả/reproducer vẫn mở.
