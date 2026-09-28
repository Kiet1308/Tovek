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
| | M0: tìm các chỗ phụ thuộc thứ tự băm | | đang làm |
