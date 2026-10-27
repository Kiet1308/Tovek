# V2.8: nghiệm thu hai mục decompiler đã chọn

Hoàn tất ngày 28/10/2026 (UTC+7). Phạm vi: kiểm chứng chéo mức tối ưu và giảm từ chối bytecode hợp lệ có proof
constructor an toàn. Nhánh `v2.8-dev`; code commit `9d0636acdf1144b6f60159848451d6d0797d20a9`.
Binary ordinary release SHA256:
`90a680b532aeb17665b83d6e8981041eed506960de2d6ca8236e57eb33377416`.
Baseline V2.7: source `4880984`, nhánh bắt đầu tại `4a8f457`, binary
`7be28396f9edf542913189f985b1d85aa26e7f2d7c7e437cef185771a8b31819`.

## Kiểm chứng chéo O0/O1/O2

Harness hiện hữu chạy 80 cases × 9 tổ hợp input/output optimization × g0/g2:
**1.440 profiles = 1.367 runtime passes + 15 expected refusals + 58
compiler-sensitive profiles; 0 failed.** CI gọi cả deep review thường và
ma trận này. Mỗi report giữ source/driver/flags, tool SHA256 và quan sát runtime.

Exit code, stdout và stderr phải khớp nguyên vẹn; byte UTF-8 lỗi không bị đổi
thành replacement character. Test âm bắt thay đổi whitespace, chuỗi giống vị
trí source, thứ tự event, stderr và invalid UTF-8. Năm driver chuyên kiểm tra
lỗi chỉ bỏ tiền tố vị trí source khỏi lỗi được `pcall` bắt, giữ nội dung lỗi.

58 profiles còn khác biệt: 22 closure identity, 16 table layout, 16 vector
behavior và 4 caller-frame profiles. Chúng khớp chính xác baseline đã review
và rebuilt observations khớp source gốc biên dịch ở output optimization.
Đây **không phải runtime passes hoặc proof tương đương input bytecode**.
Gate yêu cầu đúng case/profile, source, driver, flags và cả ba observation
fingerprints; source nhạy mức tối ưu không tự tạo ngoại lệ. Missing baseline
profiles và mọi mismatch mới/đổi đều làm gate thất bại.

## Recovery và giới hạn proof

Shared open-list pass chụp giá trị prefix đúng một lần và kiểm tra key lỗi
trước hiệu ứng của field tiếp theo. Với prefix positional/keyed hỗn hợp,
constructor cuối giữ thứ tự field, static array allocation, hash capacity,
continuation index và native SETLIST call/vararg tail. Bảng kiểm tra riêng
không escape; metadata và kiểm tra giới hạn local/register cuối pipeline vẫn áp dụng.

So cùng fixtures và options với V2.7, **33 matrix profiles chuyển từ refusal
sang runtime pass**: key-errors 12, positional-prefix 18, captured-key 3.
Đó là 11 input optimization/debug profiles × 3 output optimization levels.
Cả 1.334 matrix profiles vốn pass giữ nguyên emitted source và raw runtime
observations. 58 compiler-sensitive profiles cũng giữ nguyên.

Review đã bắt candidate trung gian đọc captured key trước callback sửa key.
Luau có thể đọc owning-register key ở thời điểm store, còn incoming upvalue
được snapshot sớm; không đảo thứ tự chung cho cả hai. Guard giữ từ chối
captured local key cạnh value có hiệu ứng khi timing chưa được chứng minh,
ở cả initial và split stores. Regression riêng giữ 15 expected refusals tại
O0/g0, O0/g2, O1/g0, O1/g2, O2/g2 × 3 output levels; O2/g0 phải chạy đúng.

Shape O2 inline helper làm thay static array allocation vẫn bị từ chối.
Positional fixture dùng callback parameters để kiểm tra cùng họ đã có proof
ở cả ba mức tối ưu; không nới allocation guard. Recovery vẫn giới hạn 64
regions, 32 fields, window 64 statements và motion fuel 4096.

## Kết quả kiểm chứng trên binary cuối

| Kiểm tra | Kết quả |
|---|---|
| Rust workspace, all targets | 1.708 passed, 0 failed, 2 ignored manual scaling probes |
| Python suite | 220 passed |
| Deep review thường | 517 profiles: 487 runtime passes, 30 expected refusals, 0 failed |
| Cross-optimization | 1.440 profiles, phân loại như trên |
| Semantic round-trip | 816/816 |
| Public source profiles | 513/513 |
| Roadmap runtime | 282/282, 9 controls passed |
| Output cũ so V2.7 | 807 semantic + 513 public + 7 residual giữ từng byte |
| Rebuilt-call annotations trên inputs cũ | Giữ 894 semantic / 72 public / 38 residual |
| Review 9 output mới | 6 inferred-call annotations; 40 snapshot producers được validate |
| Fixture and Luau audit | 33/33 bước, 34 records gồm public baseline; tất cả exit 0 |

Review độc lập kiểm tra proof, output và harness. Bật producer metadata trên
cả 9 input mới giữ source từng byte; ledger validation 9/9. Output-size chỉ
thêm 9 entries, giữ nguyên 814 entries cũ và ngưỡng. Oracle chỉ thêm 3 inputs:
17 prototypes gồm 11 exact, 2 equivalent và 4 differ do checker/snapshot và
control flow; không ghi 4 differ thành đã được oracle chứng minh tương đương.
269 entries cũ giữ nguyên; không tăng capture excess.

Các lệnh audit lấy từ workflow thật và chạy cục bộ, gồm runtime/determinism,
lineage, producer/binding/capture metadata, cache, compact style, compiler
witnesses, Wasm reader, public corpus và oracle. Hai lệnh step 7 chạy riêng
khỏi runner tuần tự, cùng options/tools; log và ma trận được liên kết vào
audit. Luau pin `c2ec0d4e5ca50796ba174a7565298f59aa572268`, Rust
nightly-2026-06-15. Test/dev profile tắt debug symbols để đủ dung lượng,
vẫn bật assertions; ordinary release profile không đổi.

Evidence tại `/workspace/work/v28`: `safe-binary.json`, `workspace-safe.log`,
`python-safe.log`, `cross-opt-final-{baseline,candidate}/`,
`cross-opt-final-comparison.json`, `review-new-output-ledgers/review.json`,
`audit-1/results.json`, `final-byte-comparison.json` và
`final-{semantic,public,residual}-comparison.json`. Candidate bị bác và
counterexamples giữ trong `cross-opt-intermediate-d341/`,
`review-ssa-key-order/`, `guard-investigation/`. Metadata lớn được gzip lossless
bằng Python, kiểm SHA256 trước khi thay raw; source outputs, binaries, logs
và manifests vẫn giữ. Cleanup cache dùng Python với exact allowlist đã preview.

Không thực hiện G1/performance work, release, version bump, tag, merge main
hoặc website/web changelog. Private dumps và ba ca gốc thiếu dữ liệu vẫn chưa
kiểm chứng. Chính sách tương thích empty preallocated hash của V2.7 không đổi;
`pairs`/`next` qua mức tối ưu khác chưa được bảo đảm. Kết quả này hoàn tất hai
mục đã chọn, không tuyên bố mọi bytecode hay mọi việc của V2.8 đều hoàn tất.
