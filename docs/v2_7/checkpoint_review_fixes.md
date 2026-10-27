# Sửa các lỗi xác nhận từ Ponytail review V2.7

Ngày 28/10/2026, UTC+7. Review toàn bộ `77edf38..dde689a`; baseline code
`b798e5b`, source sửa `4880984`. Binary ordinary release SHA256:
`7be28396f9edf542913189f985b1d85aa26e7f2d7c7e437cef185771a8b31819`.

## Lỗi và cách sửa

- **Vector constants:** FASTCALL fallback tới `Vector3.new` chỉ chứng minh
  cấu hình compiler, không chứng minh constructor runtime bất biến. Generic
  Luau dùng snapshot `vector.create` đã có; chỉ Roblox client bytecode dùng
  constructor readonly để viết trực tiếp. Ba ca ghi member, ghi qua alias và
  môi trường bên ngoài có 18 profiles input O2/g0/g2 × output O0/O1/O2:
  baseline sai 8, V2.6.1 và bản sửa đạt cả 18. Giữ test Roblox key 203 và -0.
- **Numeric style gate:** nhận đúng cách formatter viết số âm, integer,
  infinity và signed NaN, kể cả literal index. Vẫn từ chối arithmetic/call
  tùy ý và local index. Có unit controls, parser thật và fixture runtime.
- **Readable conditionals:** kiểm tra 72 cột sau khi tên cuối đã ổn định;
  tính cả indentation của table, call arguments, multi-return và boolean
  groups bao quanh closure. Giữ nguyên các shared function/block subtrees
  có thể xuất hiện ở nhiều độ sâu. Các trường hợp layout bảo thủ có comment
  nêu giới hạn; compact mode giữ vị trí pass và chính sách cũ.
- **PromptTriggered:** tách khỏi `Triggered(player)`, suy luận đúng
  `PromptTriggered(prompt, player)`. Test cả hai slots trước competing field
  evidence và kiểm tra CLI O0/O1/O2/g1 trên binary cuối.

Khi kiểm tra việc dời pass, tái hiện thêm lỗi có sẵn: bỏ writes của một local
bị capture làm compiler chia sẻ closure vốn phải tạo mới. Default readable
pass dùng capture census đã có để giữ các writes ấy; census hết budget cũng
không fold declaration. Regression có cả fresh/shared controls, Copy/Ref,
O0/O1/O2 và debug profiles. Không thêm pass sửa freshness lần hai.

## Kiểm chứng trên binary cuối

| Kiểm tra | Kết quả |
|---|---|
| Rust workspace, all targets | 1.704 passed, 0 failed, 2 ignored manual scaling probes |
| Python suite | 213 passed |
| Fixture and Luau audit | Đủ 33/33 bước; 34 records gồm public baseline |
| Semantic round-trip | 807/807 |
| Deep review | 499/499 |
| Pinned public sources | 513/513 |
| Roadmap runtime suite | 276/276; 9 controls passed |
| Oracle / output-size baseline | Đạt; không đổi entry cũ, chỉ thêm fixtures mới |

Luau pin `c2ec0d4e5ca50796ba174a7565298f59aa572268`, Rust
nightly-2026-06-15. Rust test profile tắt debug symbols để đủ dung lượng;
assertions vẫn bật. Binary release dùng profile thông thường của repo.
Audit chạy workflow thật, gồm v12/v14, compact, Wasm/Worker, source/SSA
lineage, producer/capture/binding metadata, cache và compiler witnesses.

## Review output và baseline

Đối chiếu bytes với `b798e5b`: 801 semantic và 7 residual outputs cũ không
đổi. Public giữ 510/513 outputs; ba profile O0/O1/O2 của
`RbxUtil/modules/shake/init.luau` giữ declaration/writes của captured
`timeFunction` thay vì if-expression. Tổng +21 dòng, +15 dòng không trống;
không đổi số call khôi phục ở từng file (894 semantic / 72 public / 38
residual trên các inputs cũ).

Output-size baseline chỉ thêm 6 outputs của hai semantic fixtures mới;
không đổi bất kỳ entry cũ hay ngưỡng gate nào. Oracle baseline chỉ thêm hai
source fixtures (267 → 269 inputs, 327 → 329
non-equivalent prototypes); mọi entry cũ giữ nguyên. Cả hai prototype mới
khác ở `main`: captured-choice bỏ boolean materialization trước nhánh;
final-names được compiler tối ưu các immediate reads từ private table
constructors. VM và source review đạt; đây không phải chứng minh equivalence
bằng oracle. Lượt gate ban đầu và báo cáo oracle được giữ trong
`review-fixes/{oracle-initial.log,oracle-baseline-review.json}`; lượt chạy
lại với baseline bổ sung đạt.

Evidence tại `/workspace/work/v27`: `audit-27/results.json`, logs và reports
trong cùng thư mục; `review-fixes/workspace-complete.log`,
`review-fixes/build-complete.log`, `review-fixes/complete-binary.json`,
`review-final-{semantic,residual,public}-comparison.json` và patches.
Before/after reproducers nằm trong `review-helper-vector`,
`review-ssa-control`, `review-validation-format`, `review-names-types` và
`review-fixes`. Metadata lớn được gzip lossless và kiểm tra SHA256; raw
source outputs, baseline binaries và logs giữ nguyên. Chỉ dọn cache qua
Python với exact allowlist đã preview, sau khi build/test dừng.

## Giới hạn giữ nguyên

Không chạy lại broad stress 135.000 profiles hoặc benchmark bốn version trên
binary sửa review này; các báo cáo cũ vẫn thuộc đúng checkpoint ghi trong đó.
G1 và ba original review cases thiếu dữ liệu vẫn là backlog đã được chủ repo
chấp nhận. Không có bốn private dumps/9.987 inputs hoặc Windows timing.
Empty preallocated hash giữ compatibility policy, chưa bảo đảm pairs/next
qua mọi mức tối ưu; open SETLIST chưa có proof vẫn bị typed refusal.
Không release, tag, merge main hoặc làm website/web changelog.
