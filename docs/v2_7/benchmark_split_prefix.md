# Benchmark ordinary release tại checkpoint split prefix

Ngày 28/10/2026, UTC+7. Source `d3e64a2`, binary SHA256
`8c5d3feb9facb0a3dc050e855da3fd68cf50401faf0752be96cccf24b548fb96`.
**G1 chưa đạt**: vẫn còn regression so với V2.5.1 trên corpus public khả dụng.

513 input cố định, `--key 1 --strict-no-synthetic-control`, ordinary release
cùng Rust nightly-2026-06-15/build flags; mỗi binary một warmup và 25 samples,
thứ tự đảo mỗi vòng. Ghim CPU theo 1/4 workers, filesystem và output directory
ấm; artifact cache, metadata và profiler tắt. Không chạy build/CI/fuzz đồng thời.
Mọi sample xuất đủ 513 files, deterministic trong từng version.

| Binary | Commit | Median 1 worker, s | Median 4 workers, s | Median RSS 1/4 workers, MiB |
|---|---|---:|---:|---:|
| V2.5.1 | `fe6fef9` | 0.653813 | 0.187951 | 30.00 / 54.42 |
| V2.6.1 | `77edf38` | 0.717513 | 0.208407 | 31.11 / 53.55 |
| Điểm xuất phát V2.7 | `76212c9` | 0.713553 | 0.202706 | 29.17 / 55.25 |
| Checkpoint hiện tại | `d3e64a2` | 0.719516 | 0.208825 | 33.40 / 61.46 |

Median hiện tại chậm hơn V2.5.1 10,0% / 11,1% ở 1/4 workers; so V2.6.1
là 0,3% / 0,2%, so điểm xuất phát V2.7 là 0,8% / 3,0%. Không gọi các chênh
lệch nhỏ là speedup. Tỷ lệ từng cặp cùng vòng bổ sung thông tin về nhiễu:

| So với | 1 worker: median ratio; thắng/25 | 4 workers: median ratio; thắng/25 |
|---|---|---|
| v251 | 1.1071; 5/25 | 1.1120; 1/25 |
| v261 | 1.0027; 12/25 | 1.0428; 10/25 |
| start_v27 | 1.0073; 11/25 | 1.0015; 11/25 |

RSS là process high-water đọc theo polling, không phải allocation count. Số đo
này tách khỏi checkpoint `02173c3`; không lấy biến động giữa hai lần đo làm
speedup của sửa table. Hai candidate census và SSA relevance đã bị loại sau
51 samples xác nhận, chi tiết ở [dead ends](../performance/dead_ends.md).
[CPU profile](checkpoint_cpu_profile.md) chỉ dùng chẩn đoán, không thay A/B.

Toàn bộ commands, hashes, samples, min/max/p95 và logs ở
`/workspace/work/v27/v27-final-split-benchmark-t{1,4}.json` và thư mục cùng tên;
runner `run_final_version_benchmark.py` gọi `scripts/benchmark_v2.py`.
Không có bốn private dumps hoặc timing Windows. Chưa đóng G1 và goal V2.7.
