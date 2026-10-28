# V2.8: khôi phục bốn helper sites còn lại

Hoàn tất ngày 28/10/2026 UTC. Code `d6ffc29544d35d3d1ad4a7a29ccef5f3168fb210`
đã commit/push lên `v2.8-dev`. Đợt này tiếp tục
[incoming-read checkpoint](incoming_read_timing.md), baseline code `6f14911`
và binary SHA-256 `778005572654c3edce6d243bfb99cc1a2d31692b2e36ffd4128e9ab80719bd8e`.
Binary ordinary release mới:
`b08e65c4a4521b6b99b991fbcdac22638f8e3fa8a5b8d61c539c7f88dbb46e83`.

## Kết quả và phạm vi proof

Ba fixture semantic hiện hữu, input O2/g1, có đủ bốn call còn thiếu ở chế độ mặc định:

| Fixture | Helper | Trước → sau |
|---|---|---|
| `deinline_closure_factories` | `refresh` | 0 → 2 |
| `deinline_returns_distributed_and_short_circuit` | `isCallable` | 3 → 4 |
| `exact_fraction_constants` | `show` | 24 → 25 |

Closure snapshot chỉ chuyển vào argument liền kề khi temporary có một lần
ghi/đọc, không bị capture, mọi capture cố định và proof thứ tự đánh giá cho
phép. Giữ nguyên Function/prototype, thứ tự capture và metadata; giữ các
binder cần cho helper discovery. Census thiếu, shared body, orphan context,
reassignment, protected binding và escaping DUPCLOSURE vẫn chặn recovery.

Matcher nhận thêm đúng shape `local m = P and E; if m then T else return K end`
so với nhánh P tường minh. Temporary phải riêng tư, không capture/gán lại;
mọi đường đều return và nhánh false trả literal. Dùng budget, transactional
bindings và liveness checks hiện hữu; không sửa AST nguồn để ép khớp.
`M.run` được khôi phục, `M.loose` giữ logic truthiness gốc.

Numeric-literal policy được truyền từ option emission hiện hữu qua census
và motion proof. PI/Inf/NaN chỉ được coi là literal khi chắc chắn không đổi
sang library lookup. Các API cũ vẫn bảo thủ; lookup thật, callback, capture,
vector, lỗi nil/NaN key và budget không được miễn kiểm tra. Với
`--assume-standard-libraries`, `show` vẫn có 24 calls và PI giữ spelling
`math.pi`; chế độ này cũng qua runtime O0/O1/O2 trong standard environment.

## Kiểm chứng trên binary cuối

| Kiểm tra | Kết quả |
|---|---|
| Rust workspace, all targets | 1.774 passed, 0 failed, 2 manual probes ignored |
| Python suite | 253 passed |
| Fixture audit theo workflow thật | 33/33 steps; 34 records gồm public baseline, tất cả exit 0 |
| Cross-optimization | 1.710 = 1.638 runtime passes + 24 expected refusals + 48 compiler-sensitive; 0 failed |
| Semantic VM | 834 = 799 runtime passes + 35 expected refusals |
| Public / residual inventories | 486 outputs + 27 refusals / 5 outputs + 2 refusals; giữ nguyên |

Thêm chín AST tests và ba integration tests với bytecode từ compiler đã ghim.
Sau full workspace run, chỉ tăng độ chặt assertion của integration test
`M.run`/`M.loose` rồi chạy lại cả ba tests thành công; production source và
binary không đổi. Native all-targets và `phase-allocation-trace` checks qua.
Test/dev tắt debug symbols, giữ assertions; ordinary release profile không đổi.

Ma trận thêm hai fixture hiện hữu: 36 profiles gồm 30 runtime passes và sáu
O1 refusals đã có ở binary baseline. Không phát sinh coverage loss mới;
giữ nguyên 48 compiler-sensitive records và mọi ngưỡng/baseline oracle.
Probe bổ sung O2/g1 cho ba fixture có tám runtime passes và một giới hạn
closure identity O2→O0 cũ ở `mk`. Cả trước/sau đều cho cùng ba observations;
source gốc biên dịch O0 cũng cho kết quả đó. Probe này không phải runtime
pass và không tạo thêm CI exemption.

Stress 12.000 profiles giữ nguyên từng phân loại so với baseline: broad
9.000 = 8.040 passed + 879 refused + 81 invalid; mutation 3.000 = 2.322
passed + 549 refused + 129 invalid, gồm 1.478 mutations. Không có failure.
Đây là bounded grammar với lựa chọn output optimization của generator;
invalid/refused không được tính thành runtime passes.

## Review output và bằng chứng

Trên 799 semantic outputs, chín files đổi spelling và inferred-call
annotations tăng đúng 848→852. Những đổi spelling khác là closure snapshot
chỉ đọc và callee snapshots cạnh literal NaN/Inf. Mọi input bytes,
refusal inventory và runtime observations được giữ nguyên. Toàn bộ 486 public
và năm residual outputs giữ nguyên từng byte, cùng 40/38 annotations.

Metadata của 290 roadmap và 486 public inputs giữ nguyên source, sidecar,
event identities, producers và omissions. Kiểm tra riêng ba fixture đích
qua parser và corruption controls: chỉ thêm `refresh("x")`, `refresh("y")`,
`isCallable(callback)` và `show("pi", 3.141592653589793)`; giữ mọi event/call
occurrence cũ. Review độc lập không thấy blocker trong phạm vi đã kiểm tra.

Oracle gate qua baseline cũ: semantic non-equivalent prototypes giảm
506→504, không có per-file regression hoặc capture-excess increase.
Residual oracle không đổi. Token-likeness trung bình giảm nhẹ 0,7776→0,7775
khi bỏ saved callee aliases; dataflow statuses và capture counts giữ nguyên.
Các prototypes còn differ/missing/extra vẫn
chưa được static oracle chứng minh tương đương; 77 public proof gaps từ
checkpoint trước không thay đổi vì public outputs giữ nguyên.

Evidence ở `/workspace/work/v28/helper-followup/`: `binary-release.json`,
`final-provenance.json`, `workspace-summary.json`, `integration-final.log`,
`integration-fixtures.json`, `release-helper-verification.json`,
`stress-summary.json`, `final-byte-comparison.json`,
`final-metadata-comparison.json`, `final-oracle-comparison.json`,
`targeted-metadata/review.json`, `final-output-review.md`,
`final-output-review-manifest.json` và `final-verification.json`.
Workflow reports ở `/workspace/work/v28/audit-7/`. Luau pin
`c2ec0d4e5ca50796ba174a7565298f59aa572268`, Rust `nightly-2026-06-15`.

Audit step 32 từng bị ngắt vì hết inode; log lỗi được giữ và bước đó đã chạy
lại thành công. Metadata cũ được gộp/nén lossless, kiểm SHA-256 trước khi Python
xóa bản rời; cache chỉ dọn bằng Python theo exact allowlist đã preview.
Sources, binaries, toolchains và mọi evidence bytes được bảo toàn.

Không mở G1, release, version/tag/merge main hoặc web changelog. Private dumps
và Roblox runtime của public corpus vẫn chưa kiểm chứng. Giữ chính sách
empty preallocated hash và các giới hạn compiler đã công bố; kết quả này
hoàn tất bốn helper sites, không chứng minh mọi bytecode.
