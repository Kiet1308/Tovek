# Broad stress tại checkpoint split prefix

Ngày 28/10/2026, UTC+7. Source `d3e64a2`, ordinary release SHA256
`8c5d3feb9facb0a3dc050e855da3fd68cf50401faf0752be96cccf24b548fb96`.
Không chạy build, benchmark hoặc profiler đồng thời.

| Bộ kiểm tra | Passed | Refused | Invalid | Failed |
|---|---:|---:|---:|---:|
| 15.000 seeds × 9 profiles | 133.906 | 0 | 1.094 | 0 |
| 1.000 seeds × 9 profiles, bật mutation | 7.695 | 904 | 401 | 0 |
| Structuring probes | 512 | 0 | 0 | 0 |
| Repeat/inlined-return probes | 512 | 0 | 0 | 0 |

So từng profile với `02173c3`: broad run có đúng 103 chuyển đổi refusal →
passed thuộc 31 seeds; mutation run có đúng 5 chuyển đổi refusal → passed.
Mọi trạng thái còn lại giữ nguyên. Các refusal được khôi phục đều là open
SETLIST; không đổi tiêu chí so sánh hoặc loại bỏ seed để đạt kết quả.

Mutation run thực sự sửa 4.529/9.000 profiles theo cùng seed stream cũ. 904
refusal còn lại gồm 488 NaN payload không thể spell bằng source, 341 builtin
name mismatches, 51 iterator mismatches và 24 method names không spell được.
Harness chỉ mutate tên chuỗi và NaN payload; đây không phải fuzz mọi dạng
bytecode hỏng. Invalid là source/reference không chạy được trong giới hạn
compile/VM/timeout/memory của harness, không phải input đã qua decompiler.

496/512 repeat probes có compiler-inlining witness; 16 còn lại là controls.
Hai bộ probes mở rộng coverage cho repeat/return và generic-for, nhưng không
nhận diện hoặc thay thế original review inputs đang thiếu. Broad grammar hữu
hạn tránh phụ thuộc layout tùy ý; các layout probes và VM comparisons riêng
được ghi tại [checkpoint code](checkpoint_split_prefix.md). Các open SETLIST
ngoài proof giới hạn vẫn có thể bị từ chối.

Runner `run_split_prefix_stress.py` gọi `scripts/fuzz_roundtrip.py` với
`--seed-start 0 --profiles 9 --workers 4 --timeout 20 --reduce`, thêm
`--mutate` ở lượt thứ hai. Luau pin `c2ec0d4e5ca50796ba174a7565298f59aa572268`.
Evidence tại `/workspace/work/v27`: `split-prefix-stress.log`,
`fuzz-split-prefix-{full,mutations}.json`,
`fuzz-split-prefix-{status,mutation}-comparison.json`,
`structure-probes-split-prefix/results.json` và
`repeat-inline-probes-split-prefix/results.json`, kèm commands và logs.

README nêu rõ ngoại lệ empty preallocated hash theo quyết định của chủ repo:
giữ output tương thích, chưa bảo đảm thứ tự `pairs`/`next` khi biên dịch ở mức
tối ưu khác. Không dùng ngoại lệ đó để bỏ proof của populated constructors.

Broad stress khả dụng đã hoàn tất trên binary này. G1 vẫn chưa đạt theo
[benchmark](benchmark_split_prefix.md); chưa có quyết định thay tiêu chí.
Không có private dumps, Windows timing, đặc tả case (c) hoặc original review
reproducers. Chưa đóng toàn bộ goal V2.7.
