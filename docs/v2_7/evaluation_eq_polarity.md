# L3: giữ polarity EQ hiện tại

Ngày: 27/10/2026, UTC+7. Baseline: `506d27d`.

Đã thử đổi riêng `LOP_JUMPIFEQ` thành AST `NotEqual`, đưa fallthrough thành Then và jump target thành Else. Không đổi thứ tự hai operand, các relational opcode hoặc policy NaN. Candidate qua 786 semantic, 445 deep-review cases, fixed-seed typed fuzz (206 pass, 23 typed refusal, 11 invalid), strict residual fixtures, public 513/513 và oracle. Đây là các check phục vụ đánh giá; không tuyên bố đã chạy full workspace/33 audit steps cho candidate bị loại.

| Corpus | Output đổi | Tổng dòng trước → sau | Không rỗng trước → sau | Calls trước → sau |
|---|---:|---:|---:|---:|
| Semantic | 13/786 | 47.968 → 47.992 | 40.973 → 40.994 | 875 → 875 |
| Public | 32/513 | 40.608 → 40.608 | 31.990 → 31.990 | 72 → 72 |
| Residual | 0/7 | 3.826 → 3.826 | 3.070 → 3.070 | 38 → 38 |

Đọc golden diff cho thấy lợi ích không đều: `Fusion/doCleanup` và `roact/createReconciler` bớt nesting, `WaitFor.Custom` đưa return sớm ra trước; ngược lại `Fusion/Disassembly`, `isSimilar`, `Rodux/Store` và `roact/Component` tăng nesting hoặc thêm nhánh. Các guard lifecycle vốn dùng equality/or trở thành chuỗi inequality/and, rồi lồng thêm if. `Promise._all` đẩy body dài vào một tầng if thay vì giữ guard-return ngắn.

Hai semantic fixtures `captured_swap_cell_phi` và `cell_phi_other_variable_folded` cùng O0/O1/O2 tăng bốn dòng mỗi output: điều kiện ghép tách thành hai if và lặp lại spelling `tostring(p)`. Runtime vẫn đúng, mỗi đường chỉ gọi một lần; đây là regression readability, không phải kết luận miscompile. Oracle nonequiv giảm 317 → 313 nhờ chính hai fixtures đó; số này không đủ để biện minh đổi bố cục chung.

Quyết định: loại global polarity switch và giữ implementation trước thí nghiệm. L3 là mục đánh giá, đã có diff và quyết định; không mở tùy chọn mới hay nới baseline để nhận thay đổi. Mục tiêu guard/weight có thể được cải thiện bằng proof cục bộ khi có counterexample cụ thể, không cần đảo mọi EQ jump.

Evidence tại `/workspace/work/v27`: `l3-{semantic,public,residual}-comparison.json`, toàn bộ patch/output trong `l3-audit`, `l3-rejected.patch`, binary `luau-lifter-l3-rejected`. Code được khôi phục byte-for-byte về `506d27d`; oracle/output-size baseline không thay. Không có frozen private dumps để đánh giá private golden diff.
