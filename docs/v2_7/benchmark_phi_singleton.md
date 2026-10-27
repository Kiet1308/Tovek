# Benchmark ordinary release tại checkpoint singleton phi

Ngày 28/10/2026, UTC+7. Source `b798e5b`, binary SHA256
`f1f019b6564b9447774b622ffac023155fcef3c23bb7b4d3acfe13adf5b5b094`.
**G1 chưa đạt** trên 513 input public khả dụng.

Cùng protocol với [checkpoint split prefix](benchmark_split_prefix.md):
Rust nightly-2026-06-15, cùng ordinary release flags, một warmup và 25 samples
mỗi version, thứ tự đảo mỗi vòng, CPU affinity theo 1/4 workers. Artifact
cache, metadata và profiling tắt; filesystem/output directories ấm. Không
chạy build, CI, fuzz hoặc profiler đồng thời. Mọi sample xuất đủ 513 files,
deterministic trong từng version.

| Binary | Commit | Median 1 worker, s | Median 4 workers, s | Median RSS 1/4 workers, MiB |
|---|---|---:|---:|---:|
| V2.5.1 | `fe6fef9` | 0.695920 | 0.199765 | 29.89 / 54.33 |
| V2.6.1 | `77edf38` | 0.778544 | 0.221079 | 30.96 / 55.23 |
| Điểm xuất phát V2.7 | `76212c9` | 0.798835 | 0.222195 | 29.07 / 55.19 |
| Hiện tại | `b798e5b` | 0.803912 | 0.230458 | 33.22 / 59.53 |

Median hiện tại cao hơn V2.5.1 **15,5% / 15,4%** ở 1/4 workers, so V2.6.1
là 3,3% / 4,2%, so điểm xuất phát V2.7 là 0,6% / 3,7%.

| So với | 1 worker: median paired ratio; thắng/25 | 4 workers: median paired ratio; thắng/25 |
|---|---|---|
| v251 | 1.20108; 3/25 | 1.15667; 0/25 |
| v261 | 1.04476; 8/25 | 1.00637; 8/25 |
| start_v27 | 1.02016; 9/25 | 1.00359; 11/25 |

Nhiễu lớn ở một worker: current min 0,684650 s, p95 1,166183 s, max
1,225888 s. Không dùng khác biệt giữa hai lần đo bốn version để suy speedup
hoặc regression của một thay đổi code. Phép A/B riêng của thay đổi singleton
phi, gồm lượt thăm dò trái chiều và hai lượt 51 samples, được giữ ở
[checkpoint](checkpoint_phi_singleton.md); lợi ích một worker chưa được xác
nhận. RSS là process high-water theo polling, không phải allocation count.

Raw samples, hashes, commands, min/max/p95 và logs tại
`/workspace/work/v27/v27-phi-singleton-benchmark-t{1,4}.json` và các thư mục
cùng tên; runner `run_phi_singleton_versions.py` gọi `scripts/benchmark_v2.py`.
Không có private corpus hoặc Windows timing. Tiêu chí G1 chưa được thay đổi,
goal V2.7 vẫn mở.
