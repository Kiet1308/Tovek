# Đối chiếu các phần M4/M6 đã có

Ngày đối chiếu: 27/10/2026, UTC+7. Đây là audit implementation đang có trên `v2.7-dev`, không phải danh sách tính năng vừa viết lại. Các mục chưa đủ đặc tả và nghiệm thu cuối của goal vẫn được giữ riêng.

## M4 S1

- `recover_guard_continue` chỉ chuyển trailing guard trong thân `for`/`while`; giữ scope của `repeat`. Ngưỡng condition cost là 24; gộp guard có cùng terminal action bị chặn ở 4 disjuncts hoặc cost 48. Việc tách/gộp giữ thứ tự và short-circuit.
- `flatten_guards` dùng weight để chọn terminal arm, giữ elseif và implicit bare return. Tail cloning giới hạn cost 12; table/closure có cost vượt ngưỡng. `factor_common_tails` không đưa tail qua declaration trong branch prefix; differential reference kiểm tra đường rescan.
- `negate` chỉ bỏ `not` hoặc đảo equality, không đổi `<` thành `>=`. `normalize_conditions` giữ chính sách NaN mặc định và dùng facts primitive một lần ghi, theo thứ tự source. Không dùng tên builtin làm proof cho truthiness hay phép tính.
- SSA comparison orientation giữ operand order của equality có thể gọi `__eq`; các phép đổi orientation cần proof primitive hoặc quy tắc VM đúng với toán tử tương ứng.
- `refine_names` có snapshot-noise refinement; midpoint/half chỉ là evidence yếu. `recover_methods` giữ ba veto: parameter bị gán lại, parameter khác tên `self`, hoặc body đã dùng binding `self`. Các test `keeps_dot_when_param_reassigned`, `keeps_dot_when_later_param_is_self`, `keeps_dot_when_body_mentions_self` giữ các trường hợp này.
- `fold_tuple_copies` chỉ hấp thụ temporary riêng, đọc một lần, không capture và không gộp các destination trùng nhau. Các test `stores_right_after_a_tuple_call_become_one_assignment`, `a_swap_through_a_temporary_becomes_one_assignment`, `a_temporary_read_elsewhere_keeps_the_declaration` bao phủ tuple, swap và veto.
- `deinline::Enclosing` lưu declaring depth, vòng lặp, các statement sau block và điều kiện `until`. Liveness của residue tính cả lần lặp tiếp theo khi declaration ở ngoài loop; không chỉ kiểm tra phần tail trong block hiện tại.

“Handle splits without case (c)” chỉ xuất hiện trong roadmap và goal, không có định nghĩa case (c) trong source hay các plan khả dụng. D4 đã có kiểm tra riêng cho service/module handles trong SSA inliner, `inline_temps` và `deinline::declared_by_source`; không coi đó là bằng chứng hoàn tất một phép biến đổi chưa được mô tả.

## M6 L4

`expr_deinline` đã dùng một đường `run` với chế độ expression/arithmetic; unification dùng chung `deinline`, cùng các guard capture, effect, arity, frame và budget. Legacy implementation chỉ còn dưới `cfg(test)` làm reference. Test `expression_pass_matches_legacy_targets_rewrites_origins_and_events` chạy 64 seeds ở cả hai mode, so target eligibility/order, cây/source, origins, reconstruction events và ownership. Các test tiếp theo so capture proof và giới hạn target count với reference.

`deinline::HelperCache` cache theo function và binder. `forget_rewritten` bỏ helper khi body của nó hoặc function nằm trong nó bị sửa. Capture/frame facts được đọc lại; target của helper không đổi được lấy lại thay vì clone. Một phiên bản body cũ chỉ được giữ cho hosted value helper trong trường hợp đã có proof rewrite. Không thay cơ chế này bằng cache toàn cục thiếu invalidation, không bỏ productive-round census để lấy speedup.

Vì vậy L4 cần ghi nhận và kiểm chứng cơ chế hiện hữu; không cần viết một engine khác. Mục hiệu năng G1 vẫn phải có số đo end-to-end riêng, kể cả khi L4 đã có đủ hạ tầng.

## Phần naming đã có

`refine_names` chỉ chuyển phi evidence khi cả hai nhánh cho cùng tên và các điều kiện writes/capture phù hợp; disagreement không lấy tên của nhánh đầu. API table Roblox có version `roblox-event-roles-v1`, chỉ gợi ý tên. Literal `.Name`, tuple result roles và rehoist headroom đã có test. Việc bỏ suy luận class chỉ từ `__index`, bổ sung irregular plurals và các thay đổi output liên quan được kiểm chứng ở checkpoint naming riêng.

Các test trên nằm trong workspace gates; runtime, semantic và metadata suites vẫn phải đạt ở commit nghiệm thu. Audit này không thay thế frozen dumps, các fuzz seed riêng chưa được cung cấp hoặc benchmark V2.5.1/V2.6.1.
