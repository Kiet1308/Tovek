# Copy captures at reconstructed calls

Ngày kiểm chứng: 27/10/2026, UTC+7. Baseline: `0007889`.

## Lỗi và proof

Fuzz seed 7388, O2/g1, cho thấy hai giá trị dùng chung một local sau SSA destruction: snapshot mà một helper capture bằng `Copy`, và giá trị mới của local sau một phi. Matcher so sánh local identity và dựng nhầm call của helper trả snapshot cũ. Với input `-3, true`, ca rút gọn phải trả `-2` ở kết quả thứ hai nhưng baseline trả `-3`. Cả `76212c9`, `927885c` và `0007889` đều có lỗi này.

CaptureSafety nay ghi các upvalue được capture bằng Copy theo function identity. Cả statement và expression target admission yêu cầu chúng không bị gán lại. Biến điều khiển numeric/generic for cũng là biến bị ghi lại; census đầy đủ ghi nhận chúng. Mọi capture của một function dùng chung đều được xét, không chỉ closure gặp đầu tiên. Ref captures giữ nghĩa đọc cell hiện tại; stable Copy captures vẫn được phép. Orphans vốn không có captures vẫn theo cơ chế hiện tại.

Đây là proof bảo thủ: nếu local có thể bị ghi lại ở bất kỳ nơi nào, target bị từ chối cả ở site trước lần ghi đó. Chỉ mở rộng khi có proof mọi đường chạy từ lúc tạo closure đến site. Không thay bằng giả định từ line census, tên hay thư viện chuẩn.

## Regression

`deinline_copy_capture_rebound.luau` rút gọn từ seed 7388, có pinned O2/g1 bytes cho integration test và được đăng ký deep review. Ca gốc và ca rút gọn đều đã so VM trước/sau; output biên dịch lại ở O0/O1/O2 giữ đúng kết quả sau bản vá.

Unit tests bao gồm stable Copy, reassigned Copy, Ref, nhiều closure dùng chung function, cả hai loại for và census hết budget. Expression admission kiểm tra cả nhánh nhận và nhánh từ chối.

Đợt fuzz trước bản vá gồm 15.000 seed, 9 profile mỗi seed: 133.905 passed, 1.094 invalid, 1 mismatch; không có refusal hoặc panic được báo. Invalid là input gốc không chạy hợp lệ theo harness, không tính là passed. Đây là số đo trước bản vá, không phải một đợt fuzz đầy đủ sau bản vá.

## Kiểm chứng và output

- Workspace: 1.658 passed, 2 ignored.
- Full Fixture and Luau audit: đủ 33 bước passed; 714/714 semantic cases và 301/301 deep-review cases. Gate output-size được chạy lại sau khi thêm baseline cho fixture mới.
- So 711 semantic outputs cũ với baseline: 2 file thay đổi; 41.351 → 41.354 dòng, 34.854 → 34.856 dòng không trống, 812 → 810 comment call được suy ra. Hai call mất là target có Copy capture bị ghi lại: `cell_written_in_loop_read_after__O2` và `value_capture_written_after_closure__O2`. Đây là chi phí bảo thủ của proof sửa lỗi, không nâng baseline để che mất call. Các call còn được dựng vẫn giữ comment.
- Public: 513/513 passed; cả 513 output byte-identical với `0007889`, 38.629 dòng, 31.808 dòng không trống, 72 comment inferred call.
- Oracle: 238 inputs, 1.540 prototypes; toàn bộ 237 dòng baseline cũ giữ nguyên status/nonequiv/protos. Fixture mới có 5 prototypes đều exact/equivalent, nonequiv tổng vẫn 295. Output-size và oracle baseline gates đã chạy lại, đều passed.

Không có bốn private frozen dumps hoặc timing trên máy Windows gốc. Checkpoint này không đóng toàn bộ goal v2.7; M6 và các mục mở khác tiếp tục trong GOAL.md.
