# CPU sampling tại checkpoint split prefix

Ngày 28/10/2026, UTC+7. Source `d3e64a2`; dùng riêng bản release có symbols
(`cargo rustc --release -p luau-lifter --bin luau-lifter -- -C strip=none`).
Không dùng bản diagnostic này để tính speedup so với release phân phối.

`perf 6.12.107` thu `cpu-clock:u` ở 999 Hz, DWARF stack 8.192 bytes, một worker
và ghim một CPU. Chạy 20 lượt đủ 513 public inputs; 14.924 samples, không mất
sample. Mọi output giống từng byte với ordinary binary của `d3e64a2`.

Chi phí tự thân phân tán: `_mi_page_malloc_zero` 4,19%, `mi_free` 2,65%,
`RValue::visit_local_reads` 2,45%, `decompile_function` 2,04%, SSA destruct
1,83%, `Inliner::inline_rvalues` 1,56%, SSA construct 1,47%. Theo call stacks,
SSA construct chiếm khoảng 9,48% gồm con, inline 7,73%, destruct 7,18%,
statement deinline 5,82%, naming 5,40%. Các tỷ lệ gồm con chồng nhau, không
cộng chúng thành tổng. Đây là tỷ lệ samples trên workload này, không phải
latency theo pha hoặc speedup dự kiến.

Không rebuild line tables cho toàn bộ libraries; một số inline frames hoặc
stack sâu không được phân giải hết. Báo cáo call graph dùng `--no-inline`
để tránh addr2line chậm. Profiler không thay kiểm chứng release A/B; G1 vẫn mở.

Raw capture, logs, reports đã demangle và provenance/hashes nằm tại
`/workspace/work/v27/perf-split-prefix*`; script chạy corpus là
`run_perf_public.py`. Binary có symbols được giữ riêng, output public đã đối
chiếu với binary thường. Công cụ perf và libraries được giải nén từ gói Debian
đã kiểm tra SHA256 vào thư mục work, không thêm dependency vào repository.
Không có private corpus hoặc timing trên máy Windows.
