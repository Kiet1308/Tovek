# M6 L1/L2: source-informed spacing and proven return types

Ngày kiểm chứng: 27/10/2026, UTC+7. Baseline: `79ecacf`.

## Thay đổi

L1 giữ SourceLines từ bytecode line metadata qua AST origin và các bản sao function. Khoảng cách giữa các statement có vị trí đáng tin dùng line gap; bytecode không có line info và control syntax dựng lại dùng heuristic cũ. Range đảo chiều/chồng lấn không được coi là proof khoảng cách. SourceLines không tham gia PartialEq hay debug fingerprint. Line spans được giữ inline, không cấp phát từng node; kích thước Statement trên 64-bit tăng từ 128 lên 144 byte. Đây là chi phí feature được đo, không phải tối ưu byte-identical.

L2 chạy sau mọi biến đổi expression/condition, trước final naming và formatter. Chỉ in return pack được chứng minh từ final AST: nil, boolean, number, string, union của các loại này và tuple có cùng arity trên mọi đường có thể return. Implicit fallthrough góp pack rỗng. Return từ call/local, toán tử có thể dùng metamethod, pack khác arity hoặc control chưa có proof giữ nguyên không annotation. Type hint của parameter, tên API Roblox và tên builtin không phải proof. Closure vô danh, named function và colon method dùng chung cách in annotation.

Không khôi phục được comments hay whitespace gốc chính xác từ bytecode. Source lines định hướng khoảng cách; vị trí thiếu hoặc bị rewrite không được bịa thêm. Return types hiện không tạo schema cho table/function/vector hoặc suy luận type qua các call chưa được chứng minh.

## Fixture và gates

`source_spacing.luau` có call sát nhau, call có khoảng cách và nested closure; pinned O2/g1 và O2/g0 kiểm tra cả metadata và fallback. `proven_return_types.luau` có union/tuple/implicit return, số lượng return khác nhau, parameter hint sai với metamethod, thay thế math và method/anonymous function; pinned O0/g1 và O2/g1 kiểm tra header được phép/từ chối.

- Workspace: 1.665 passed, 2 ignored.
- Semantic: 720/720; deep review: 313/313.
- Full Fixture and Luau audit: đủ 33 bước passed, gồm Wasm/Worker và metadata/runtime/determinism controls. Output-size và oracle gates đã chạy lại sau khi bổ sung baseline cho hai fixture mới.
- Oracle: toàn bộ 238 dòng baseline cũ giữ nguyên status/nonequiv/protos. Hai fixture mới có nonequiv 4 và 1; chạy cùng source bằng binary `79ecacf` cũng cho đúng các số này. Tổng nonequiv 295 → 300 chỉ do thêm coverage, không có per-file regression. Bounded-oracle nonequiv không phải bằng chứng runtime sai hoặc proof tương đương đầy đủ.

## Output review

So 714 semantic outputs cũ: 610 file thay đổi; 41.474 → 40.917 dòng, 34.949 dòng không trống giữ nguyên, 810 inferred-call comments giữ nguyên. Mọi diff chỉ là dòng trống và primitive return annotations.

So 513 public outputs: 403 file thay đổi; 38.629 → 40.375 dòng, 31.808 → 31.817 dòng không trống, 72 inferred-call comments giữ nguyên. Dòng trống tăng có chủ đích theo source metadata. Ba output O0/O1/O2 của RbxUtil `modules/wait-for/init.luau` thêm ba dòng không trống mỗi file: annotation `: boolean` khiến call `PromiseWatchAncestry` vượt giới hạn chiều rộng và formatter xuống dòng arguments. Đã đọc diff ba ca này; bỏ annotations và whitespace thì token stream của toàn bộ 513 output không đổi.

Không coi tổng dòng public giảm là tiêu chí của source-spacing: feature này phải giữ được cả source gaps trước đây bị bỏ mất. Không hạ proof hay bỏ call để giảm dòng. Oracle và output-size baseline được cập nhật sau review, giữ nguyên chính sách gate.

## Chi phí public release

513 input public đã compile, manifest và binary SHA256 lưu cùng raw evidence tại `/workspace/work/v27`. Dùng release cùng toolchain/flags, không cache/profiling, filesystem ấm, tính cả ghi output; một warmup rồi bảy sample xen kẽ A/B mỗi worker count. Ghim CPU bằng taskset; mỗi repeat kiểm tra hash của toàn bộ output. Hai binary có output khác nhau do feature, nhưng mỗi binary phải deterministic. Peak RSS đo riêng từng process bằng wait4.

| workers | trước, median | sau, median | sau/trước | peak RSS trước/sau, median |
|---|---:|---:|---:|---:|
| 1 | 0,654244 s | 0,666473 s | 1,01869 | 28.680 / 32.816 KiB |
| 4 | 0,190456 s | 0,189707 s | 0,99607 | 53.464 / 57.540 KiB |

Không tuyên bố speedup: sample ngắn trên cloud có nhiễu, một luồng có chi phí tăng khoảng 1,9%, bốn luồng gần như ngang nhau. Profiling chạy riêng trên cùng public corpus; return-type pass chiếm 2,14 ms exclusive trong lượt instrumented sau. Phase timing không thay cho A/B release. Đây chưa phải benchmark cuối với V2.5.1/V2.6.1.

Không có private frozen dumps hoặc Windows timings của chủ repo. L4 và các mục roadmap còn mở tiếp tục sau checkpoint này; không release hay sửa website.
