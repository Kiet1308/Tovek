# CPU time bổ sung cho hai candidate SSA

Ngày 28/10/2026, UTC+7. Đo ordinary binaries bằng protocol public 513 inputs,
25 samples mỗi label/worker count, xen kẽ đảo thứ tự, CPU affinity 1/4 workers,
warm filesystem/output directories, artifact cache tắt. Không chạy workload
build/CI/fuzz/profile đồng thời; mọi output của cả ba binary giữ từng byte.

Bản sao của `scripts/benchmark_v2.py` ở thư mục work thêm chênh lệch
`resource.getrusage(RUSAGE_CHILDREN)` trước spawn và sau wait. Trong khoảng đó
runner chỉ có một child; CPU time cộng user+system của các thread trong child,
không tính polling/hashing ở Python parent. Đây là số đo hệ điều hành, không
phải instrumentation trong engine. CPU time không phải elapsed latency và có
thể lớn hơn elapsed khi chạy nhiều workers.

| Binary | Median wall 1t / 4t, s | Median CPU 1t / 4t, s |
|---|---:|---:|
| Split prefix `d3e64a2`, `8c5d3feb…` | 0.749464 / 0.220124 | 0.729173 / 0.810500 |
| Singleton phi `b798e5b`, `f1f019b6…` | 0.766373 / 0.218698 | 0.735883 / 0.809954 |
| Empty-cell gate bị loại, `73145408…` | 0.764584 / 0.211631 | 0.739011 / 0.794810 |

So singleton phi với split prefix, CPU median một worker tăng 0,9%; paired
CPU ratio 1,02437, chỉ 8/25 cặp nhanh hơn. Bốn workers có paired CPU ratio
0,99251, thắng 15/25. Các bootstrap paired-median 95% percentile intervals
đều chứa 1. Lượt này không xác nhận speedup một worker, nhất quán với giới hạn
đã nêu trong [checkpoint](checkpoint_phi_singleton.md). Giữ toàn bộ evidence,
kể cả số đo trái chiều; không suy hiệu quả code từ hai lần benchmark khác nhau.

Empty-cell gate cũng không có lợi ích một worker so singleton phi: paired CPU
ratio 1,00640, thắng 11/25. Bốn workers có ratio 0,99280, thắng 17/25; interval
[0,96815; 1,00142] vẫn chứa 1. Gate đã hoàn nguyên, ghi cả ordinary A/B và lý
do tại [dead ends](../performance/dead_ends.md). Không chạy full audit cho gate
bị loại; source/binary cuối vẫn là checkpoint `b798e5b` đã audit đủ.

Evidence tại `/workspace/work/v27`: `benchmark_with_cpu.py`,
`run_ssa_cpu_benchmark.py`, `ssa-cpu-r25-t{1,4}.json`,
`ssa-cpu-paired-analysis.json` và logs. Bootstrap dùng 10.000 resamples, seed
27, chỉ chẩn đoán độ nhiễu trên cùng corpus/cloud. G1 vẫn chưa đạt theo
[benchmark bốn version](benchmark_phi_singleton.md); không có private hoặc
Windows timing claim.
