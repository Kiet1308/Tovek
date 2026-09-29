# Tovek V3 engine

Tài liệu sống: mục tiêu, số đo, quyết định kiến trúc, lộ trình và tiến độ của
engine V3. Branch: `v3-engine` (tách từ `main` @ `4d7df00`). Chưa push, chưa release.

## 1. Mục tiêu

- Decompile **một script bình thường** nhanh nhất có thể, mục tiêu **10x** so với
  v2.2. Đo là thời gian decompile thực trong tiến trình (như web server và
  `decompile-folder` gọi), không tính khởi động tiến trình.
- Chất lượng output giữ nguyên. Mỗi bước được kiểm chứng **giống hệt từng byte**
  với golden của bước trước trên corpus 3.978 file, cộng toàn bộ test Rust.
- Tính năng khác (chế độ analysis của Volt, cache, PGO) để sau.

## 2. Số đo xuất phát

Máy: Intel Core Ultra 9 275HX (8 nhân P + 16 nhân E). Đo một luồng ghim nhân P
(CPU 12). Script lấy theo phân vị kích thước bytecode của corpus
(`D:\Medal\v22-work\samples\pNNN.luac`), đo nóng trong tiến trình bằng
`D:\Medal\v22-work\latbench` (tốt nhất trong 40 lần):

| Script | Bytecode | Hàm | Thời gian (main @ `4d7df00`) |
|---|---:|---:|---:|
| p010 (Cmdr command) | 234 B | 1 | 0,018 ms |
| p025 (CameraUI) | 585 B | 3 | 0,39–0,44 ms |
| p050 (Processors/Player) | 1,6 KB | 3 | 0,72–0,79 ms |
| p075 | 3,9 KB | | 0,84 ms |
| p090 | 8,6 KB | 11 | 2,7–3,7 ms |
| p099 | 23,6 KB | | 8,3 ms |
| p100 (LightningCore) | 83 KB | | 93 ms |

Corpus toàn bộ (fat LTO): 1 luồng 9,14 s, 24 luồng 0,94 s (v2.2: 11,50 s / 1,08 s).

Với script trung vị: pha SSA theo hàm ~40–45%, ~40 pass cấp chunk ~55–60%, mỗi
pass vài chục µs. Không có điểm nóng.

## 3. Chẩn đoán

Đã đo và loại trừ:

| Giả thuyết | Thí nghiệm | Kết quả |
|---|---|---:|
| Locality của AST | Sao chép lại cây cho liền mạch trước các pass chunk | 0% |
| Khóa/atomic của `RcLocal` | Thay `Mutex<Local>` bằng ô không đồng bộ | ~1% |
| Allocator | mimalloc v2 / v3 / direct TLS | ±0% |

Nguyên nhân thật là **phong cách code của pass**, lặp lại ở khắp nơi: một script
30 dòng (p025) tốn ~0,45 ms, tức khoảng 200–500 ns cho mỗi nút mỗi pass, trong khi
một lượt duyệt cây Box tốn vài ns mỗi nút. Các mẫu tốn kém:

1. `values_read()` / `rvalues()` / `stmt_rvalues()` cấp phát `Vec` ở mỗi nút.
2. `FxHashMap<RcLocal, _>` với `entry(local.clone())` ở mỗi lượt đọc (clone, băm,
   dò bảng, rồi drop).
3. Census (`collect_usage`, closures, tên dành trước, capture) tính lại ở mỗi pass,
   nhiều chỗ tính lại cho từng block, từng hàm lồng nhau.
4. ~59% thời gian pass chunk là lượt chạy không thay đổi gì.
5. Pha SSA chạy lại toàn bộ các bước tới khi hội tụ (trung bình 4,3 vòng/hàm).
6. Visitor `dyn FnMut`, clone cây để thử.

Ràng buộc: output phụ thuộc thứ tự duyệt `FxHashMap<RcLocal, _>` ở một số chỗ
(thí nghiệm đổi seed băm: 140–160/3.978 file đổi, toàn là khác biệt về vị trí
khai báo hoặc thứ tự nhánh). Điều này trói mọi thay đổi cấu trúc dữ liệu.

## 4. Kiến trúc V3

V3 giữ nguyên định nghĩa kiểu AST (`ast::Statement`, `RValue`, …) để mọi bước có
thể kiểm chứng độc lập, không cần cầu nối, và thay toàn bộ phần bên dưới:

- **Thứ tự quyết định tất định theo id local** thay cho thứ tự băm (M0).
- **Index local dày bằng số học**: id local đã có dạng `base + func_idx·2^40 + k`,
  nên index dày tính được không cần băm. Các fact theo local nằm trong mảng
  (`LocalVec<T>`) thay vì `FxHashMap<RcLocal, T>`.
- **Duyệt không cấp phát**: visitor generic (đơn hình hoá), không trả `Vec`.
- **Fact dùng chung**: census (đọc/ghi/capture/khai báo) tính một lần và dùng lại
  cho các pass không thay đổi cây.
- **Gác ứng viên**: mỗi pass kiểm tra điều kiện cần rẻ trước khi làm việc nặng.
- **Viết lại các pha theo phong cách hiệu năng**, ưu tiên theo chi phí: lõi SSA
  (dựng SSA, inliner, destruct, structuring), restructure, rebuild bảng/inline temp,
  đặt tên, de-inline, formatter, lifter.

## 5. Lộ trình

| Mốc | Nội dung | Tiêu chí |
|---|---|---|
| M0 | Chuẩn hoá thứ tự: các chỗ duyệt map theo `RcLocal` có ảnh hưởng output chuyển sang thứ tự id | Output không đổi khi đổi seed băm; golden mới qua mọi gate chất lượng; khác biệt so với v2.2 chỉ là thứ tự |
| M1 | Nền tảng: `LocalIndex`/`LocalVec`, visitor generic, fact dùng chung | Giống hệt golden M0 |
| M2 | Viết lại từng pha/pass (SSA core trước) | Mỗi bước giống hệt golden M0; đo tốc độ từng bước |
| M3 | Điều phối pipeline: gác theo dạng, song song theo hàm nơi có thể | Giống hệt; đo cuối so với v2.2 |

Gate cho mỗi commit: corpus 3.978 file giống hệt (`D:\Medal\v22-work\tools\iter.sh`),
test Rust của crate bị sửa. Gate đầy đủ (deep review, semantic, runtime,
oracle, compact) chạy ở mốc M0 và cuối mỗi mốc.

## 6. Công cụ

- `tools/iter.sh`: build nhanh (không LTO), decompile corpus 24 luồng, so với golden.
- `tools/bench.py`: benchmark corpus một luồng ghim nhân P, xen kẽ các binary.
- `latbench/`: đo độ trễ từng script trong tiến trình (`LAT_RUNS`, `LAT_THREADS`,
  `MEDAL_PROFILE_JSON` cho telemetry theo pass).
- `tools/buildprof.sh` + `profile.sh` + `scope.py` / `tree.py`: sampler stack.
- `tools/ed.py`: sửa file giữ kiểu xuống dòng (có file trộn CRLF/LF).

## 7. Tiến độ

| Ngày | Việc | Commit | Kết quả |
|---|---|---|---|
| 2026-09-29 | Tối ưu trên `main` trước V3 (gác ứng viên, dedupe, FxHash, liveness bitset, …) | `88cc56a`…`985df98` | 1,26x một luồng, 1,15x 24 luồng; giống hệt từng byte |
| 2026-09-29 | Đo độ trễ từng script, chẩn đoán, thiết kế V3 | tài liệu này | |
| 2026-09-29 | M0: một chỗ duy nhất gây phụ thuộc thứ tự băm (thứ tự seal tham số phi trong SSA). Sửa theo thứ tự tạo phi | `7f456fe` | 3 seed băm khác nhau cho output giống hệt nhau; 158 file đổi một lần (chỉ thứ tự khai báo/số của biến tạm, số dòng không đổi); mọi gate qua; golden mới `corpus-m0` |
| 2026-09-29 | Bộ đếm `prof` ns dùng chung mọi crate; `latbench` đo từng pha trong tiến trình (script mẫu và cả corpus) | `430c626`, `d1e0d36`, … | |
| 2026-09-29 | M1: `ast::dense` (slot local tính từ id, không băm); đổi tên SSA dùng bảng dày | `c8f8be7` | Đổi tên SSA: 38→33 µs (p50), 230→210 µs (p90) |
| 2026-09-29 | Gate `deinline` khi không có helper đủ điều kiện cấu trúc | `b7e7b9c` | p90: 67→13 µs |
| 2026-09-29 | M2: dominator tức thời dạng mảng (Cooper–Harvey–Kennedy) thay `simple_fast`; test ngẫu nhiên so với petgraph | `3daaabe` | |
| 2026-09-29 | M2: cây dominator + duyệt CFG dạng mảng trong out-of-SSA | `d81dde8` | |
| 2026-09-29 | M2: đếm lượt đọc dạng mảng (`Usages`) trong SSA inliner | `eca6267` | |
| 2026-09-29 | M1: duyệt con biểu thức bằng visitor, không cấp phát `Vec` mỗi nút (inventory ngân sách cây, capture safety, refine tên, synth helper, …) | `6661c9a` | corpus trong tiến trình 7,14→6,92 s |
| 2026-09-29 | Một lượt duyệt cho bất biến cuối (goto/label + marker vòng lặp); visitor cho lượt đọc local ở link upvalue, deinline, phụ thuộc tham số | `542b57b` | 6,92→6,78 s |
| 2026-09-29 | **So với bản phát hành V2.1.1** (tag `v2.1.1`, cùng máy, chạy xen kẽ; `main` từ đây gọi là V2.2) | `58b969b` | 1 luồng 10,25 → 7,80 s (**1,31x**); 24 luồng 1,06 → 0,93 s (**1,14x**), tổng CPU 14,1 → 10,9 s (1,29x). Mốc "v2.2" trong các dòng dưới là bản trình bày cũ trên `main` local, vốn chậm hơn V2.1.1 |
| 2026-09-29 | **Đo cuối (release fat LTO, corpus 3.978 file)** | `542b57b` | 1 luồng: v2.2 12,02 s → M0 9,62 s → **V3 8,07 s (1,49x)**; 24 luồng: 1,18 → 1,04 → **0,92 s (1,28x)**. Giống hệt từng byte với `corpus-m0` |

### Thí nghiệm đã loại

| Ý tưởng | Kết quả | Lý do bỏ |
|---|---|---|
| Arena bump theo từng lần decompile (giải phóng = no-op) | 3–7% | Rủi ro đối tượng thoát arena (thread-local, bộ đệm nội bộ rayon/std) lớn so với lợi ích |
| Thay mọi bảng băm bằng mảng | ~10% ở pha đổi tên SSA | Băm không phải chi phí chính; giữ làm hạ tầng |
| `get_mut` trước `entry(local.clone())` trong census (tránh tăng/giảm refcount atomic) | 0% trên corpus | Atomic của `RcLocal` không đáng kể |
| Tập `stable_captured` của inline_temps chỉ giữ local bị capture | 0% | Chi phí nằm ở lượt duyệt, không ở insert |
| Cờ "cả chunk không có goto" tính một lần cho inline_temps | lợi ~45 ms, tốn ~60 ms để tính | Chỉ có lời nếu bỏ luôn kiểm tra bất biến cuối, mà đó là lưới an toàn |
| Bỏ lượt quét "không đổi" cuối của SSA inliner (~0,35 s) | không chứng minh được | Census mới mỗi lần gọi có thể mở cơ hội inline mới; bỏ qua sẽ đổi output |

### Bản đồ chi phí (corpus 3.350 script duy nhất, trong tiến trình, 1 luồng: 7,17 s)

Pha theo hàm 3,59 s: inliner 0,88 (vòng lặp chính 0,64, census 0,10), dựng SSA 0,81 (đổi tên
0,36), destruct 0,55, restructure 0,45, khai báo local 0,12, dominator 0,10. Phần chunk
~3,5 s: rebuild bảng 0,48, đặt tên 0,42, lift 0,33, deinline 0,33, format 0,23, inline temps
0,21, normalize 0,18, refine tên 0,17, cleanup_final 0,13. Cấp phát: 41,5 triệu lần/4,5 GB
mỗi vòng (~14–15% thời gian).

Kết luận: chi phí nằm ở **lượng việc** của thuật toán (vòng lặp chạy lại, phân tích tính lại,
duyệt cây nhiều lần), không ở cấu trúc dữ liệu. Hướng tiếp theo: bỏ việc lặp lại trong từng
thuật toán mà giữ nguyên kết quả.

### Đo thêm sau M2 (corpus trong tiến trình, 1 luồng: 6,78 s)

- **Pass chunk chạy mà không đổi gì: ~1,2 s/7,1 s (17%)** (đo bằng dấu vân tay `Debug` của cây
  trước/sau mỗi pass, `tools/fp_instrument.patch`). Lớn nhất: `inline_single_use_temps` (98% lượt
  không đổi, ~216 ms), `normalize_conditions` (~126 ms), `cleanup_final` (~106 ms),
  `rehoist_constants` (~101 ms), `branch_constructors`/`synthesize_terminal_helpers` (không bao giờ
  đổi trên corpus, ~64/62 ms). Phần lớn chi phí là census toàn cây trước khi biết không có gì để
  làm; muốn gác chính xác cần điều kiện cần rẻ riêng cho từng pass, và nhiều pass không có điều
  kiện như vậy (ví dụ inline temp cần đếm lượt dùng mới biết).
- Một lượt duyệt toàn cây tốn ~30 ms trên corpus (~6 ns/nút) — đã sát giới hạn của cây `Box` +
  visitor. Phần chunk ≈ số lượt duyệt × 30 ms; ~50 pass × 2–3 lượt.
- SSA inliner: lượt quét cuối không đổi ~0,35 s; block được quét lại mà không đổi chỉ ~0,12 s
  (18%) — phần lớn là việc inline thật.

## 8. Đánh giá

Mục tiêu 10x **không đạt được** dưới ràng buộc output giống hệt từng byte. Hồ sơ chi phí phẳng:
không pha nào quá ~12%, và mỗi pha đã là thuật toán hợp lý chạy trên một cây AST mà hàng chục pass
lần lượt duyệt. Mọi thay đổi cấu trúc dữ liệu (băm → mảng, arena, allocator, khoá/atomic) chỉ cho
vài phần trăm. Trần thực tế của hướng "viết lại chính xác từng pha" ước khoảng 2x so với v2.2;
V3 hiện ở 1,49x (1 luồng) / 1,28x (24 luồng).

Muốn vượt xa hơn cần một trong các hướng không còn giữ nguyên từng byte:

1. **Hợp nhất pass**: gộp các pass chunk cùng loại (census + viết lại) thành ít lượt duyệt hơn.
   Thứ tự áp dụng quy tắc thay đổi nên output có thể khác (cần gate chất lượng thay cho so byte).
2. **IR mới cho phần chunk** (mảng phẳng thay cây `Box`/`Mutex`), viết lại toàn bộ ~50 pass.
   Khối lượng rất lớn; lợi ích chủ yếu là hằng số duyệt.
3. **Cache theo hàm/script** (đã để sau theo yêu cầu): script trùng lặp giữa các game (thư viện
   bundle) có thể bỏ qua toàn bộ pipeline — đây là cách duy nhất cho hệ số lớn trên web server.

## 9. Nghiên cứu hướng khác (sau khi bỏ hướng chấp nhận output thay đổi)

Hướng "bỏ/gộp pass, chấp nhận output thay đổi" đã thử trên `v3-engine` (ablation từng pass):
chỉ một pass thừa thực nghiệm (`inline_single_use_temps` riêng, ~3%); không merge.

| Hướng | Cách đo | Kết quả |
|---|---|---|
| PGO (profile-guided optimization) | Train trên 1.774 input benchmark V2.1, đo corpus Roblox; thêm lần train trên chính corpus | 0–2% (kể cả khi train trên chính bộ đo); loại |
| Cache theo hàm (proto) | Khoá = code + hằng số + hình dạng + con (đệ quy); đếm proto đã gặp ở script trước (`tools/proto_dup.py`) | 12,5% proto nhưng chỉ 4,1% số lệnh trùng; trung vị mỗi file 0%. Cache nguyên file (đã có) mới có ích, tuỳ tần suất lặp |
| Chế độ analysis (Volt `decompile_all`, `--emit-upvalue-analysis`) | Corpus 3.978 file, 24 luồng | 5,0 s so với 0,9 s chế độ thường (5,6x), 381 MB sidecar. Profile 24 luồng: rename 33%, tạo file 15%, `canonicalize` trong `validate_analysis_scripts_root` 12% (mỗi file một lần), flush 10%, close 8%; decompile ~7% |

Bỏ fsync riêng lẻ: 1 luồng 31,8→25,1 s nhưng 24 luồng không đổi — ở 24 luồng nút thắt là
metadata của hệ thống file (temp + rename từng file trong cùng thư mục), không phải fsync.
Hướng đề xuất: publish nguyên cây output một lần (ghi thẳng vào thư mục staging rồi đổi tên
thư mục), kiểm tra gốc `scripts` một lần mỗi lần chạy, giữ handle thư mục; giữ nguyên nội dung
output và các bảo đảm chống thoát đường dẫn.
