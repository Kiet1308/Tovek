# Các hướng tối ưu hiệu năng đã thử nhưng không đáng làm

Danh sách gộp mọi hướng tối ưu đã **đo** và bị loại, từ 2026-06 đến 2026-10-04, kèm con số
và nguồn. Mục đích là không đề xuất và đo lại cùng một ý tưởng ở các vòng sau.

Quy tắc:

- Chỉ đề xuất lại một hướng ở đây khi có bằng chứng **mới**, tức là profile cho thấy chi
  phí đã dịch chuyển so với lúc đo.
- Khi một hướng mới bị loại, thêm một dòng kèm con số đo và nguồn.
- Mọi số liệu là đo, không phải ước lượng, trừ khi ghi rõ.

## Bối cảnh: hồ sơ chi phí phẳng

Đo trên corpus Roblox (xem [v3_engine.md](../v3_engine.md) §2–§3 và §7):

- Không có điểm nóng: pha SSA theo hàm chiếm ~40–45%, ~40–50 pass cấp chunk chiếm ~55–60%.
  Không pha nào vượt ~12%.
- Một lượt duyệt toàn cây tốn ~30 ms trên corpus (~6 ns/nút), đã sát giới hạn của cây
  `Box` + visitor. Phần chunk xấp xỉ bằng số lượt duyệt × 30 ms.
- ~17% thời gian pass chunk là lượt chạy không đổi gì. Phần lớn chi phí đó là census
  toàn cây trước khi biết không có gì để làm, nên không gác rẻ được.
- Chi phí nằm ở **lượng việc** của thuật toán, không ở cấu trúc dữ liệu. Vì vậy các thay
  đổi biểu diễn bên dưới đều chỉ cho vài phần trăm hoặc không gì cả.

## 1. Biểu diễn dữ liệu và bộ nhớ

| Hướng | Kết quả đo | Nguồn |
|---|---|---|
| Locality của AST: sao chép cây cho liền mạch, IR dạng arena | 0% | [v3_engine.md](../v3_engine.md) §3 |
| Arena bump cho mỗi lần decompile (giải phóng là no-op) | 3–7%; rủi ro đối tượng thoát arena (thread-local, bộ đệm rayon/std) | v3_engine.md §7 |
| Khóa/atomic của `RcLocal`: thay `Mutex<Local>` bằng ô không đồng bộ | ~1% | v3_engine.md §3 |
| `get_mut` trước `entry(local.clone())` để tránh refcount atomic | 0% | v3_engine.md §7 |
| Allocator khác ngoài mimalloc (mimalloc v2/v3/direct TLS) | ±0%. mimalloc so với allocator hệ thống là ~1,5x và được giữ; jemalloc không hỗ trợ MSVC | v3_engine.md §3 |
| Thay mọi bảng băm bằng mảng | ~10%, chỉ trong pha đổi tên SSA; giữ làm hạ tầng | v3_engine.md §7 |
| Compact `RValue` (enum nhỏ hơn) | Thời gian chậm hơn 1,30% | [2026-09-27-followup.md](2026-09-27-followup.md), mục Compact RValue |
| Early SSA sealing | Loại: đổi output và xung đột với guard provenance | 2026-09-27-followup.md, mục Early SSA sealing |
| Chuyển sang compact IR sớm (W13) | Loại trước khi làm: số đo giảm statement lấy ở giai đoạn sau, không áp dụng được | 2026-09-27-followup.md, W13 |
| Băm local theo phạm vi / thêm finalizer (W14) | Chỉ có mô hình đếm thao tác, không đo được speedup | 2026-09-27-followup.md, W14 |

## 2. Thuật toán và pass

| Hướng | Kết quả đo | Nguồn |
|---|---|---|
| Chỉ giữ local bị capture trong `stable_captured` của inline_temps | 0%: chi phí nằm ở lượt duyệt, không ở insert | v3_engine.md §7 |
| Cờ "cả chunk không có goto" cho inline_temps | Lợi ~45 ms, tốn ~60 ms để tính | v3_engine.md §7 |
| Bỏ lượt quét "không đổi" cuối của SSA inliner (~0,35 s) | Không chứng minh được giữ nguyên output: census mới có thể mở cơ hội inline mới | v3_engine.md §7 |
| Tránh clone entry | 0% | nhánh `v3-engine` |
| Tính lại dominator trong vòng lặp của `decompile_function` | Chỉ ~2%. Tính một lần thì không an toàn vì đồ thị thay đổi | nghiên cứu độ trễ một script, 2026-06 |
| Index `last_occ` của deinline dựng sẵn từ đầu | Chậm hơn: dựng 9.717 bảng mỗi script. Phải dựng lazy (10–23 lần, <1 ms) | nghiên cứu độ trễ một script, 2026-06 |
| Bỏ pass, chấp nhận output thay đổi (ablation từng pass) | Chỉ `inline_single_use_temps` riêng là thừa (~3%); không merge | v3_engine.md §9 |

## 3. Build và toolchain

| Hướng | Kết quả đo | Nguồn |
|---|---|---|
| `codegen-units=1`, `target-cpu=v3/native`, thin LTO, `debug=false` | <3%, trong nhiễu | nghiên cứu độ trễ một script, 2026-06 |
| PGO | W7: 5,11% / 3,48%, dưới ngưỡng 10% đặt trước. Đo lại: 0–2%, kể cả khi train trên chính bộ đo | 2026-09-27-followup.md W7; v3_engine.md §9 |
| `panic = "abort"` | Cấm: mỗi hàm được cô lập bằng `catch_unwind` | `luau-lifter/src/lib.rs` |
| Binaryen cho worker wasm | 2,5–3% ở các lượt lặp, chậm hơn ở lượt đầu; giữ `--no-opt` | 2026-09-27-followup.md, mục Worker |

## 4. Cache và song song

| Hướng | Kết quả đo | Nguồn |
|---|---|---|
| Cache theo hàm (proto) | 12,5% proto nhưng chỉ 4,1% số lệnh trùng giữa các script; trung vị mỗi file 0%. Cache nguyên file (đã có) mới có ích | v3_engine.md §9 |
| Song song theo closure trong pipeline AST | Không làm được: `name_locals` cần tập tên dành trước của cả chương trình | nghiên cứu độ trễ một script, 2026-06 |

## 5. Ghi file

| Hướng | Kết quả đo | Nguồn |
|---|---|---|
| Xóa rồi tạo lại mọi file output (chống ghi xuyên symlink/hard link) | Chậm hơn 24% khi ghi đè một cây output có sẵn trên Windows (0,91 → 1,13 s, 24 luồng). Thay bằng `create_new`; file đã có mà không bị alias thì truncate tại chỗ, bằng tốc độ cũ | commit `9d39a89` |
| Bỏ fsync từng file ở chế độ analysis (`--emit-upvalue-analysis`) | 1 luồng 31,8 → 25,1 s nhưng 24 luồng không đổi: nút thắt là metadata hệ thống file (temp + rename từng file) | v3_engine.md §9 |

## 6. Bẫy khi đo

- **Chỉ so hai binary build cùng cấu hình.** Bản build incremental so với bản build
  thường trông như chậm đều ~5% ở *mọi* pass, kể cả pass không đổi code. Build lại cùng
  cấu hình thì hai bên bằng nhau (2026-10-04).
- **Máy nhiễu.** Đo một luồng không ghim nhân dao động ±50%, và tốc độ máy trôi trong
  ngày (cùng một binary: 8,07 s rồi 4,33 s). Chỉ tin phép đo xen kẽ, ghim nhân P (công cụ
  ở [v3_engine.md](../v3_engine.md) §6).
- **Chọn đúng mốc so sánh.** Con số "1,49x" của V3 là so với một bản v2.2 local vốn chậm
  hơn release. So với release v2.1.1: 1,31x (1 luồng), 1,14x (24 luồng).
- **Ước lượng theo kiến trúc thường hứa quá.** [v2_5_engine_research.md](v2_5_engine_research.md)
  dự báo 2,5x–4x; thực đo của engine V3 là 1,31x so với release. Chỉ báo số đo.

## Hướng còn mở (cần quyết định, chưa bị loại)

Các hướng này chưa được đo là vô ích. Chúng phá ràng buộc output giống hệt từng byte,
hoặc đang được để sau:

- Gộp pass, hoặc IR mới cho phần chunk. Output có thể đổi nên cần gate chất lượng thay cho
  so byte ([v3_engine.md](../v3_engine.md) §8).
- Cache nguyên script trên web server, cho các thư viện bị bundle lặp giữa các game (§8).
- Chế độ analysis: publish cả cây output một lần (staging rồi đổi tên thư mục), kiểm tra gốc
  `scripts` một lần mỗi lần chạy (§9).
