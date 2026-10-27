# Chi phí formatter và cấp phát tên trên public corpus

Ngày: 27/10/2026, UTC+7. Feature baseline: `506d27d`.

## Profile trước tối ưu

Ordinary release và profile được chạy riêng. Profile trên 513 inputs public ghi tổng exclusive 814 ms; SSA inline 64,8 ms, function 57,9 ms, decompile 57,7 ms, naming 45,2 ms, structuring 44,5 ms, lift 45,9 ms, format 22,9 ms. D_SCAN inclusive 33,9 ms, collect targets 16,2 ms; census mỗi vòng chỉ 0,95 ms/21 calls. Không dùng phase timing làm số đo speedup. D_SCAN của V2.5.1 chưa tách các child spans nên không so exclusive theo tên mới với số 0 của version cũ.

## Candidate formatter

`wants_blank_line` từng đọc lại line metadata của statement ở cả hai ranh giới liền nhau. Cache một cặp index/range trong mỗi `format_block_no_indent`; index phải trùng statement thực tế sau trailing comments hoặc merged local-function definition mới tái dùng. Không giữ AST/local owners, không cache qua block hay qua mutation. Test kiểm tra known/unknown source lines, leading/trailing comments và hai statement ghép thành local function.

Mọi output public giống từng byte trước/sau ở tất cả samples. Cùng release flags/toolchain, affinity 1/4 CPU, warmup riêng, xen kẽ binary; profile/cache metadata tắt.

| Ordinary release run | Median 1 thread trước → sau | Median 4 threads trước → sau |
|---|---|---|
| 7 samples, output directory mới | 0,715598 → 0,708606 s (-1,0%) | 0,200393 → 0,198982 s (-0,7%) |
| 25 samples, output directory ấm | 0,799027 → 0,771051 s (-3,5%) | 0,220343 → 0,228326 s (+3,6%) |
| 51 samples xác nhận, output directory ấm | 0,814418 → 0,792612 s (-2,7%) | 0,219424 → 0,219206 s (-0,1%) |

Lượt 51 paired samples: 29/51 thắng ở 1 thread, 28/51 ở 4 threads; median paired ratios 0,9668/0,9980. Máy cloud có nhiễu, không suy một speedup cố định từ các median này. Giữ cache nhỏ sau full validation; chưa kết luận đã hết regression so V2.5.1.

## Candidate ASCII naming

Đã thử lọc ASCII vào Vec<u8> và dùng lại buffer cho String thay cho Vec<char> rồi collect thành String. Đổi shared lower-camel helper và đủ hai callers; 1.161 AST tests đạt, gồm Unicode/acronym/keyword guards. Public outputs giữ từng byte.

Đo độc lập so với binary chỉ có cache formatter: 7 samples, median 1 thread 0,747254 → 0,743689 s (-0,5%), 4 threads 0,208713 → 0,206510 s (-1,1%). Xác nhận 51 samples: 1 thread 0,778117 → 0,770349 s (-1,0%), 4 threads 0,218694 → 0,218607 s (gần 0%). Median paired ratios lại là 1,0103/1,0012; candidate chỉ thắng 25/51 và 22/51 cặp. RSS median không giảm. Không nhận thay đổi vì chưa chứng minh được lợi ích end-to-end ổn định; code được khôi phục về baseline, patch/binary/results được lưu và hướng này được ghi trong dead_ends.

## Validation và giới hạn

Code cuối đạt 1.688 workspace tests (2 ignored), 786 semantic cases, 445 deep-review cases và đủ 33 audit steps (`audit-15`). So với `506d27d`, cả 786 semantic, 513 public và 7 residual outputs giống từng byte; oracle và output-size baselines không đổi. Binary SHA256: `6b04ffdc89ef448e215f0d92805cf41db2568adbb33a5c0353d1dae4fa3d7e02`. Chưa nghiệm thu toàn bộ G1. Không có bốn private dumps hoặc phép đo Windows gốc. Evidence tại `/workspace/work/v27`: profiles, `formatter-lines-measure`, `formatter-lines-repeat-t{1,4}.json`, `formatter-lines-confirm-t{1,4}.json`, saved binaries và patch.
