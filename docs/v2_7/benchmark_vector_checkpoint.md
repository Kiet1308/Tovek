# Benchmark ordinary release tại checkpoint vector metadata

Ngày 27/10/2026, UTC+7. Code hiện tại: `02173c3`; binary SHA256
`fe266a6b8af4ba0cb37f27019024a0436cf247b33c5ec9877a7d3f11313ebf02`.
**G1 chưa đạt**: thời gian một luồng và bốn luồng đều cao hơn V2.5.1 và
V2.6.1 trên public corpus khả dụng.

## Cách đo

513 input public cố định, `--key 1 --strict-no-synthetic-control`, ordinary
release cùng Rust nightly-2026-06-15 và build flags. Mỗi binary có một warmup
và 25 mẫu; thứ tự binary đảo sau mỗi vòng. CPU affinity cố định theo 1/4 worker;
filesystem và output directory ấm, artifact cache/metadata/profiling tắt.
Không chạy build, CI hoặc fuzz đồng thời. Mọi mẫu xuất đủ 513 files và có
cùng source hash trong từng version. Khác version không yêu cầu cùng output.

| Binary | Commit | Median 1 worker, s | Median 4 workers, s | Median RSS 1/4 workers, MiB |
|---|---|---:|---:|---:|
| V2.5.1 | `fe6fef9` | 0,672777 | 0,188938 | 28,84 / 53,27 |
| V2.6.1 | `77edf38` | 0,718987 | 0,210164 | 29,95 / 54,23 |
| Điểm xuất phát V2.7 | `76212c9` | 0,746323 | 0,209376 | 28,07 / 52,38 |
| Checkpoint hiện tại | `02173c3` | 0,742243 | 0,219276 | 34,18 / 58,51 |

So với median V2.5.1, bản hiện tại chậm hơn 10,3% / 16,1% ở 1/4 worker;
so với V2.6.1 là 3,2% / 4,3%. So với điểm xuất phát, median một luồng thấp
hơn 0,5% nhưng bốn luồng cao hơn 4,7%: không coi đây là speedup.

Median tỷ lệ từng cặp trong cùng vòng và số cặp nhanh hơn:

| So với | 1 worker: tỷ lệ; thắng/25 | 4 workers: tỷ lệ; thắng/25 |
|---|---|---|
| V2.5.1 | 1,1002; 3/25 | 1,1833; 2/25 |
| V2.6.1 | 1,0436; 8/25 | 1,0477; 7/25 |
| Điểm xuất phát V2.7 | 1,0182; 11/25 | 1,0498; 5/25 |

Máy cloud có nhiễu; không dùng phase timing hay số thao tác để thay kết quả
end-to-end. RSS là process high-water đọc theo polling, không phải allocation
count. Chưa có bốn private dumps hoặc timing trên máy Windows của chủ repo.

## Evidence

`/workspace/work/v27/run_version_benchmark.py` gọi công cụ
`scripts/benchmark_v2.py`. Toàn bộ samples, commands, tool/input/output hashes,
min/max/p95 và log được giữ tại
`/workspace/work/v27/v27-acceptance-benchmark-t{1,4}.json` và các thư mục cùng
tên. Các binary baseline vẫn được giữ riêng. Đây là checkpoint để chọn bước
tối ưu tiếp theo, không phải nghiệm thu xong mục hiệu năng hoặc toàn bộ V2.7.
