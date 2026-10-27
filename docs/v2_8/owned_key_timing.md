# V2.8: giữ đúng thời điểm đọc captured key

Code `302f9a36e49c55f2e7fc5c185ab7847aca757ad4`, nhánh `v2.8-dev`; baseline của đợt này là `9d0636a`
(tài liệu `239e90e`). Binary ordinary release SHA256:
`bf265a3363380835c5258f6e0e66bd7fa3a86a40e5c99d2b9cee4b7de6ffdcdc`.
Baseline binary: `90a680b532aeb17665b83d6e8981041eed506960de2d6ca8236e57eb33377416`.

## Thay đổi

Trong `{ [key] = change(), ... }`, khi `key` là register thuộc chính hàm
đang chạy, SETTABLE đọc key sau callback. Snapshot sớm sẽ lấy nhầm giá trị
trước khi `change()` sửa key. Bản trước giữ từ chối họ này; bản mới snapshot
value trước rồi key, chỉ khi census `CaptureSafety::register_of` đã chứng
minh ownership. Dùng proof hiện hữu; không suy ownership từ tên hay debug info.

Census xét captures ở mọi closure site trước khi bỏ qua Function đã duyệt,
nên Function dùng chung AST không được phân loại theo occurrence đầu tiên.
Function identity theo đúng lexical body được truyền qua nested blocks.
Incoming/ambiguous keys hoặc census hết budget vẫn bị từ chối. Compound key
và explicit saved-key giữ evaluation order cũ; không đảo thứ tự toàn bộ keys.
Giới hạn 32 fields, 64 regions/statements và motion fuel 4096 không đổi.

Unit controls có 16 trường hợp positive và 8 negative: root/nested function,
initial/split store, pure/effectful value, global lookup và shared-function
captures. Review độc lập kiểm tra proof, output, producer metadata và oracle.

## Kết quả trên candidate cuối

| Kiểm tra | Kết quả |
|---|---|
| Rust workspace, all targets | 1.709 passed, 0 failed, 2 ignored manual scaling probes |
| Python suite | 220 passed |
| Ma trận 82 cases × 9 Oin/Oout × g0/g2 | 1.476 profiles: 1.400 runtime passes, 18 expected refusals, 58 compiler-sensitive, 0 failed |
| Deep review thường | 529 profiles: 498 runtime passes, 31 expected refusals, 0 failed |
| Semantic round-trip | 822/822 |
| Public source | 513/513 |
| Roadmap runtime | 288/288, 9 controls passed |
| Fixture and Luau audit | 33/33 bước, 34 records gồm public baseline; tất cả exit 0 |
| Source outputs cũ | 816 semantic + 513 public + 7 residual giữ từng byte |
| Rebuilt-call annotations trên inputs cũ | Giữ 900 semantic / 72 public / 38 residual |
| Review 6 output mới | 4 inferred-call annotations; 48 snapshot producers được validate |

So cùng 1.476 profiles với baseline, thêm **33 profiles từ refusal sang pass**:
15 từ counterexample late-key cũ, 18 từ fixture owned-key mới. Ca late-key đã
chuyển thành semantic mandatory-pass, không còn refusal allowance. Cả 1.367
runtime passes cũ và 58 compiler-sensitive profiles giữ nguyên emitted source,
raw observations và fingerprints. 18 refusals hiện tại thuộc fixture incoming
mới, được ghi riêng, không phải regression của một input từng pass.

Bộ probe riêng 27 sources × 27 Oin/Oout/debug profiles: **729 = 621 runtime
passes + 108 typed refusals; 0 failed**. Phục hồi 495 profiles, giữ nguyên
126 profiles vốn pass. Refusals chỉ ở bốn incoming/grandchild variants.
Controls có nil/NaN đổi thành key hợp lệ và ngược lại, boolean key, callback
ném lỗi, index/global lookup, `__add`, callback yield, parenthesized/saved/
compound keys, split prefix và helper inlining. Source gốc của từng probe
có cùng raw observations ở cả chín input optimization/debug profiles.

Stress trên seed mới của typed fuzzer hiện hữu:

| Lượt | Profiles | Passed | Invalid | Typed refusal | Failed |
|---|---:|---:|---:|---:|---:|
| Seeds 20000–20999, đủ 9 input O/debug | 9.000 | 8.914 | 86 | 0 | 0 |
| Seeds 21000–21999, 3 profiles, có mutation | 3.000 | 2.527 | 152 | 321 | 0 |

Cả 321 refusals ở lượt sau đều thuộc bytecode đã mutation. Fuzzer này vẫn có
quy tắc chọn output optimization riêng cho các compiler-sensitive families;
không lấy stress count thay cho ma trận Oin/Oout chính xác bên trên.

Output-size chỉ thêm 6 entries, giữ nguyên 823 entries cũ và ngưỡng. Oracle
chỉ thêm 2 inputs, giữ nguyên 272 entries cũ: thêm 19 prototypes gồm 14 exact
và 5 differ. Năm raw deltas không mất instruction; chỉ thêm private checker
allocation/stores và nil loads, không thêm/mất call, capture, branch hay
native tail. Giữ `dataflow: unknown`; source review và runtime controls không
được ghi thành proof tương đương tổng quát. Capture excess vẫn bằng 0.

## Evidence và giới hạn

Evidence tại `/workspace/work/v28/followup`: `binary.json`, `workspace.log`,
`key-timing-probes/{comparison.json,candidate/}`, `cross-opt-owned-key-comparison.json`,
`stress.json`, `stress-mutations.json`, `review-new-output-ledgers/review.json`,
`final-byte-comparison.json`; full audit tại `/workspace/work/v28/audit-2/`.
Luau pin `c2ec0d4e5ca50796ba174a7565298f59aa572268`, Rust nightly-2026-06-15;
test/dev tắt debug symbols, giữ assertions; release flags thông thường.
Metadata lớn được gzip lossless, kiểm SHA256 trước khi Python bỏ raw.
Cache cleanup chỉ bằng Python với allowlist đã preview, khi không có build.

Incoming/grandchild key timing và shape O2 làm đổi static array allocation
chưa được mở recovery. 58 compiler-sensitive profiles vẫn là giới hạn đã
review, không phải runtime passes; chính sách empty preallocated hash giữ
nguyên. Không có private dumps để kiểm chứng. Không làm release, website,
version bump, merge main hay G1/performance trong đợt này.
