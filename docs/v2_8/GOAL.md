# V2.8: kiểm chứng chéo tối ưu và giảm từ chối bytecode hợp lệ

Trạng thái: hoàn tất hai mục chủ repo đã chọn và đợt sửa lỗi/review tiếp theo.
Code mới nhất `d6ffc29`; đã khôi phục cả bốn helper sites còn lại tại
[helper recovery checkpoint](helper_shape_recovery.md). Các checkpoint trước
gồm [incoming-read checkpoint](incoming_read_timing.md),
[owned-key recovery](owned_key_timing.md) và [ACCEPTANCE](ACCEPTANCE.md).
Nhánh `v2.8-dev`, bắt đầu tại `4a8f457`; engine baseline `4880984`.
Binary baseline SHA256:
`7be28396f9edf542913189f985b1d85aa26e7f2d7c7e437cef185771a8b31819`.

## Phạm vi

1. Mở rộng harness hiện hữu để kiểm tra đủ 9 tổ hợp input O0/O1/O2 ×
   output O0/O1/O2, với g0/g2, cho vector, table allocation, capture,
   closure identity, multret, thứ tự side effects và lỗi.
2. Phân loại refusals bằng source/bytecode tái hiện được. Mở rộng shared
   constructor proof cho các họ compiler-generated open SETLIST có thể
   biểu diễn đúng, thêm fixture và kiểm tra trước/sau.

G1/benchmark hiệu năng, bytecode v15/NEWCLASS, release, merge main và website
không thuộc đợt này. Ba review cases thiếu dữ liệu của V2.7 vẫn ở backlog.
Giữ Ponytail full, D1–D4, capture identity, thứ tự đánh giá và frame limits.
Không thay chính sách tương thích empty preallocated hash đã được chủ repo chốt.

## Tiêu chí kiểm chứng

- Mỗi kết quả ghi rõ input/output optimization, debug flags, compiler config
  và observations. Refusal hoặc giới hạn đã biết không được đếm là runtime pass.
- Không âm thầm đổi mức tối ưu để làm test qua. Khi khác biệt xảy ra, biên
  dịch lại cả source gốc để chẩn đoán ảnh hưởng compiler; kết quả ấy không tự
  động chứng minh output decompile đúng.
- Mọi thay đổi proof có positive/negative controls về escape/capture,
  nil/NaN key errors, giá trị snapshot, native bulk growth và layout.
- Chạy Rust workspace, Python tests và toàn bộ Fixture and Luau audit;
  so output cũ và mọi rebuilt-call annotation. Baseline chỉ cập nhật sau
  review source/runtime delta, không nới ngưỡng để bỏ qua regression.
- Commit/push từng mốc đã kiểm chứng. Dọn cache chỉ bằng Python với exact
  allowlist đã preview; giữ binaries, fixtures, source outputs và evidence.

## Tiến độ

- [x] Fetch và tạo nhánh từ V2.7 đã nghiệm thu; working tree ban đầu sạch.
- [x] Ghim baseline binary và provenance tại `/workspace/work/v28/baseline.json`.
- [x] Hoàn thiện ma trận tối ưu và regression controls: 1.440 profiles, 0 failed.
- [x] Recovery có guard và positive/negative controls: 33 profile từ refusal sang pass.
- [x] Full audit 33/33 bước, review output, cập nhật riêng baseline mới và bàn giao evidence.

## Ma trận baseline và giới hạn quan sát được

Lượt thăm dò trên V2.7 dùng 77 cases × 18 profiles: 1.325 runtime passes,
3 typed refusals và 58 mismatches. Kiểm tra độc lập xác nhận cả 58 rebuilt
observations khớp từng byte với source gốc biên dịch ở output optimization:
22 closure identity, 16 table layout, 16 vector behavior và 4 caller-frame
profiles. Đây là các khác biệt còn tồn tại, không phải runtime passes hay
proof tương đương bytecode gốc. Gate chỉ nhận đúng case/profile, source,
driver, flags và ba observation fingerprints đã review; không miễn trừ mọi
source thay đổi hành vi theo compiler.

Ma trận mới đã chặn candidate trung gian `d3412996…`: 12 runtime mismatches
và 6 refusals ở positional-prefix fixture. Review độc lập xác định nguyên
nhân đọc captured key quá sớm; candidate đó không được nghiệm thu. Giữ
counterexample và bổ sung guard trước khi tiếp tục full audit.

Lượt baseline cuối dùng cùng 80 cases × 18 profiles: 1.334 runtime passes,
58 compiler-sensitive profiles và 48 refusals. Trong đó 33 refusals thuộc
ba fixtures recovery; 15 thuộc counterexample captured-key cần giữ từ chối.
Harness so sánh nguyên vẹn exit/stdout/stderr, giữ byte UTF-8 không hợp lệ;
chỉ các driver kiểm tra lỗi cụ thể bỏ vị trí source khỏi thông báo lỗi.

Initial positional prefixes chỉ được nhận khi static array allocation và
continuation index khớp chính xác. Shape O2 inline helper làm thay allocation
vẫn bị từ chối; không nới proof để nhận shape ấy. Captured local key cạnh
value có hiệu ứng cũng không snapshot khi chưa chứng minh thời điểm đọc key.

## Kết quả đợt đầu

Code đã commit/push lên `v2.8-dev`. Rust 1.708 passed (2 manual probes
ignored), Python 220 passed; semantic 816/816; public 513/513. Ma trận
1.440 profiles gồm 1.367 runtime passes, 15 expected refusals, 58
compiler-sensitive profiles và 0 failures. Tất cả 807 semantic, 513 public
và 7 residual outputs cũ giữ nguyên từng byte. Không thay baseline entries
cũ, ngưỡng gate hoặc chính sách tương thích đã chốt.

Phần hoàn tất là hai mục decompiler của đợt này; G1, release và website
không thuộc goal. Bounded runtime/oracle suites không chứng minh mọi bytecode.

## Cải thiện tiếp theo: thời điểm đọc captured key

- [x] Dùng ownership proof hiện hữu để snapshot value trước key của register
  trong chính hàm, giữ từ chối incoming/ambiguous keys.
- [x] Chuyển late-key counterexample sang mandatory-pass; thêm controls cho
  saved/compound keys, nil/NaN, metamethods, errors, yield và shared Function.
- [x] Ma trận 1.476 profiles: 1.400 runtime passes, 18 expected refusals,
  58 compiler-sensitive profiles, 0 failed; thêm 33 profiles được khôi phục.
- [x] 729 timing probes, 12.000 stress profiles, Rust 1.709 tests, Python
  220 tests và full audit 33/33 bước đều không có failure. Invalid inputs và
  typed refusals trong stress được ghi riêng trong checkpoint.
- [x] 822 semantic / 513 public / 288 roadmap profiles đạt; toàn bộ
  816 semantic, 513 public và 7 residual outputs cũ giữ nguyên từng byte.
- [x] Code và evidence đã commit/push lên `v2.8-dev`.

[Chi tiết và giới hạn còn lại](owned_key_timing.md). Không đổi ngưỡng gate,
58 sensitivity baselines hoặc chính sách tương thích đã chốt.

## Hoàn tất: incoming reads, thứ tự gọi và O2 inlining

- [x] Chứng minh incoming upvalue theo capture identities nhất quán, giữ
  từ chối unknown/ambiguous/incomplete contexts và kiểm tra cả Copy/Ref.
- [x] Khôi phục open-list constructors với key được đọc trước value effects.
- [x] Sửa việc output O2 inline helper làm đổi early key read thành late read,
  kể cả constructor hoàn chỉnh và saved copy bị mất ở input g0/g1.
- [x] Giữ snapshot của logical operands và builtin aliases; bảo toàn thứ tự
  lookup/arguments của FASTCALL fallback qua cả SSA và helper reconstruction.
- [x] Giữ các counterexamples mới là failures cho đến khi sửa; không thêm
  compiler-sensitive exemptions. Chạy lại ma trận, runtime/oracle và full CI.
- [x] Giữ actual builtin/native-vector call identity; từ chối open tuples
  chưa có proof thứ tự thay vì phát code sai arity hoặc thêm call frame.
- [x] Khôi phục 220 CONCAT helper calls, readonly-capture/helper patterns
  có proof và naming hints của saved iterator/API callees.
- [x] Semantic oracle chạy đúng input bytecode; mọi refusal được khóa bằng
  source/profile/diagnostic và kiểm tra đủ inventory ở các consumer.
- [x] Rust 1.762 tests, Python 253 tests, 33/33 audit steps và 12.000 stress
  profiles không có failure. Ma trận 1.674 = 1.608 runtime passes + 18
  refusals + 48 compiler-sensitive profiles; bỏ 10 sensitivity entries đã sửa.
- [x] Review độc lập, ghi bằng chứng và commit/push code `6f14911`.

Checkpoint ghi riêng coverage loss do giữ thứ tự an toàn: 14 matrix profiles
trước đây pass nay bị từ chối; 35 semantic, 27 public và 2 residual profiles
được từ chối có diagnostic đã review. Public/residual corpus chưa chạy trong
Roblox. Bốn helper sites ở checkpoint này đã được xử lý trong đợt tiếp theo;
77 public static-proof gaps vẫn giữ nguyên. Không tính compiler limits/refusals
thành runtime passes.

## Hoàn tất: bốn helper sites còn lại

- [x] Khôi phục hai `refresh`, một `isCallable` và một `show("pi", …)` ở
  chế độ mặc định, giữ guards về capture, truthiness, thứ tự gọi và emission.
- [x] Rust 1.774 tests, Python 253 tests, 33/33 audit steps và 12.000 stress
  profiles không có regression mới. Ma trận mở rộng 1.710 profiles gồm
  1.638 runtime passes, 24 refusals đã biết và 48 compiler-sensitive profiles.
- [x] Review độc lập source/metadata/oracle; giữ baseline và toàn bộ public /
  residual output bytes. Code `d6ffc29` đã commit/push lên `v2.8-dev`.

Chi tiết, numeric-library mode và giới hạn g1 closure identity tại
[helper recovery checkpoint](helper_shape_recovery.md).
