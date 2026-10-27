# Table allocation, folding và helper matching

Ngày kiểm chứng: 27/10/2026, UTC+7. Baseline: `d13979e`.

## Phạm vi sửa

`TableShape` giữ cả hash capacity và array allocation từ NEWTABLE; allocation tham gia equality và shared helper matching. Các phép fold ở SSA và AST dùng chung proof cho capacity, ghi đè nil và việc chuyển field ra cuối constructor. Template chỉ nhận thêm key trong capacity cũ khi các value trước đó chắc chắn khác nil. Khi unifier đối chiếu NEWTABLE với DUPTABLE, kích thước và các value tại site phải chứng minh cùng collision layout; không dùng tên builtin hoặc type hint làm proof.

Các key số tường minh có thể ở hash part tại O0, hoặc ở array part được compiler cấp sẵn tại O1/O2. Chỉ constructor gồm key số tuần tự thực sự nằm trong array mới chuyển thành positional fields; việc chuyển giữ fixed arity của call. Constructor đã trộn positional và keyed fields giữ cách đánh số riêng của positional fields. Khi chưa có positional field, array padding đứng trước các keyed stores để không xóa giá trị của computed key trỏ vào array. Allocation gốc được giữ trước khi xét phần reservation của SETLIST sau đó, kể cả tail call trả về zero values.

Constructor có field nhưng thiếu keyed syntax để tái tạo hash capacity dùng các lần ghi nil vào chính key đầu, trước value thật; nếu array đã tồn tại thì slot 1 là chỗ giữ capacity không tạo hash key mới. Formatter in padding trực tiếp; AST không coi nó là một lần store thật và register-pressure check tính cả register của key/value. Số lượng được giới hạn, trường hợp chưa biểu diễn an toàn được từ chối trước emission.

## Quyết định cho bảng rỗng

Chủ repo chọn giữ tương thích cho bảng rỗng được cấp sẵn hash, thay vì mất output public. Compiler Luau dự đoán kích thước từ các store xuất hiện sau declaration, kể cả hình dạng `setmetatable({}, meta)`. Tái biên dịch output ở mức tối ưu khác có thể thay allocation và thứ tự `pairs`/`next`; checkpoint này không tuyên bố giải quyết hoàn toàn trường hợp đó.

Lượt kiểm tra chặt ban đầu từ chối 24 semantic cases và 79/513 public inputs. Các thử nghiệm thu hẹp giảm public refusals còn 40; sau review chủ repo chọn chính sách tương thích. Không giữ các thử nghiệm chèn key mới vào bảng rỗng, không giả định `setmetatable` là builtin bất biến, không ép optimization pragma cho cả output. Ngoại lệ chỉ áp dụng constructor rỗng; các proof cho populated constructors, fold và match vẫn giữ.

## Kiểm chứng

- Workspace: 1.679 passed, 2 ignored; semantic 771/771; deep review 415/415.
- Fixture and Luau audit: đủ 33 bước passed ở `audit-9`, gồm v9/v12/v14, Wasm, lineage, binding, cache, determinism và public suites. Hai baseline gates được chạy lại sau output/oracle review; không đổi ngưỡng gate.
- Release candidate đạt 1.380/1.380 table probes và 96/96 empty-table observer probes. Các nhóm capacity, helper layout, replacement và mixed allocation trước đó có lần lượt 93/384, 156/240, 81/240 và 64/144 mismatch; bản cuối không còn mismatch trong các nhóm này.
- 16 fixtures mới bao phủ capacity/growth, nil collision history, replacement/movement, NEWTABLE–template matching, numeric keys, array allocation, dynamic keys, zero-result tail và call arity. `table_empty_observers` bảo vệ việc không tạo key mới có thể bị `next` quan sát trước store đầu.
- Các candidate ở `audit-7`/`audit-8` từng từ chối `constructor_regions`; chúng không phải kết quả nghiệm thu. Regression được mở rộng qua cả allocation reservation và SETLIST reservation, rồi toàn bộ gates chạy lại.

## Review output và oracle

Đã đọc toàn bộ diff và các delta bổ sung của candidate cuối:

| Bộ | File đổi | Dòng trước → sau | Dòng không trống | Call comments |
|---|---:|---:|---:|---:|
| 723 semantic outputs cũ | 96 | 41.304 → 41.779 | 35.274 → 35.702 | 828 → 827 |
| 513 public outputs | 87 | 40.375 → 40.608 | 31.817 → 31.990 | 72 → 72 |
| 7 residual outputs | 2 | 3.786 → 3.826 | 3.030 → 3.070 | 38 → 38 |

Public giữ nguyên call count từng file. Phần tăng dòng đến từ capacity padding và các store phải giữ ngoài constructor. Hai semantic outputs `unfoldable_temporaries_over_local_limit` tăng 218 dòng mỗi file để giữ 219 array slots; baseline được cập nhật theo thay đổi này, không che nó bằng cách nới line gate. Các constructor không còn fold kéo theo tên local và dot/colon spelling của các pass sẵn có; runtime controls vẫn đạt.

Một call trong `table_layout_keeps_pairs_order__O2` giữ inline: helper có template ba keys và một store thêm, còn site đã là constructor bốn keys. Proof tổng quát không gộp template khi parameters có thể nil. Các constant arguments cụ thể có thể an toàn, nhưng matcher hiện chưa chứng minh được trường hợp đó; không giả vờ giữ đủ semantic call count.

Oracle trên cùng 257 sources: nonequiv 312 → 306, 1.606 prototypes. Với 241 sources cũ, tổng giữ 298: bốn rows tăng một prototype vì array nil stores hoặc store được giữ riêng; ba rows về signed-zero/layout cải thiện tổng cộng bốn. Bốn rows tăng là `deinline_copy_capture_rebound`, `deinline_written_parameter_live_outside`, `drained_field_positional_slot`, `loop_swap_with_later_capture`; tất cả qua VM checks. Trong 16 sources mới, nonequiv 14 → 8. `table_array_mixed_keys` đổi SETTABLEN sang SETLIST đúng allocation nên oracle multiset tăng 0 → 1; VM probes giữ behavior. Không có capture-excess file. Bounded oracle là công cụ triage, không thay thế runtime proof.

## Chi phí và giới hạn

Release A/B cùng toolchain, ghim CPU, artifact cache tắt, một warmup và bảy samples xen kẽ mỗi bên, 513 public inputs: median một worker 0,668130 → 0,664753 s (−0,5%); bốn workers 0,194650 → 0,185717 s (−4,6%). Peak RSS median lần lượt 32.896 → 32.776 KiB và 55.580 → 55.516 KiB. Một luồng coi là gần như không đổi; đây không phải bằng chứng đã đạt mục tiêu hiệu năng so V2.5.1.

Phần padding bổ sung bị giới hạn 255 slots mỗi allocation; constructor chưa có encoding được chứng minh an toàn vẫn trả typed refusal. Ngoại lệ tương thích cho empty hash tables ở trên còn hiệu lực. Không có private frozen dumps; không tuyên bố bảo đảm mọi layout hoặc đã nghiệm thu toàn bộ V2.7.

Evidence nằm tại `/workspace/work/v27`: `audit-9`, `table-layout-final-*-comparison.json`, các patch tương ứng, `table-layout-oracle.json`, `table-layout-before-oracle.json`, `table-*-release-accepted`, `table-empty-release-accepted` và `table-layout-measure/report.json`. Binary SHA-256: `3031bf3303e6b62b596ba02072a4dbdb2e7749a19db2723c1caf0224b9331ed5`. Lineage JSON của audit 6–8 được nén lossless bằng Python, đối chiếu SHA-256 trước khi bỏ bản gốc; các manifest `compressed-lineage-evidence-{6,7,8}.json` giữ đường dẫn và hash.

Không release, sửa website hoặc merge main. Tiếp tục các mục còn mở của goal.
