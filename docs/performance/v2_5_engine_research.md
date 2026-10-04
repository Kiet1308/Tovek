# V2.5: nghiên cứu khả năng nhanh hơn 3x–10x

Viết ngày 2026-09-28 trên `main` @ `883cf44` (đã gồm toàn bộ v2.2, chưa release).
Mục tiêu: xác định có tồn tại một bước nhảy hiệu năng lớn (3x, 5x, 10x) mà vẫn
giữ nguyên chất lượng output hay không. Đây là tài liệu nghiên cứu: mọi con số
ghi rõ là **đo** hay **ước lượng**.

> **Cập nhật 2026-10-04:** ước lượng 2,5x–4x dưới đây đã không thành hiện thực. Đo
> thực tế: locality/arena 0%, engine V3 cuối cùng 1,31x (1 luồng) so với release
> v2.1.1. Xem [v3_engine.md](../v3_engine.md) §8 và danh sách các hướng đã loại ở
> [dead_ends.md](dead_ends.md).

## 1. Kết luận

- **Không còn đòn bẩy nhỏ.** 14 vòng tối ưu trước (W1–W14, xem
  [2026-09-27-followup](2026-09-27-followup.md)) cộng lại ~1,3x; các thay đổi biểu
  diễn cục bộ (compact RValue, early SSA sealing, scoped hash) đều bị loại vì không
  nhanh hơn. Profile hiện tại xác nhận vì sao: chi phí trải đều trên ~45 pass cấp
  chunk và vòng SSA theo hàm, không có điểm nóng nào đủ lớn.
- **Đột phá chỉ có thể đến từ việc thay lõi engine**: IR dạng arena, local đánh số
  dày, không lock/Arc trong một hàm, fact (def/use, capture) được duy trì thay vì
  mỗi pass tự tính lại, pipeline AST chạy song song theo hàm. PoC trên AST thật của
  toàn corpus đo được: census nhanh hơn **9,2–9,6x**, deep copy nhanh hơn **23x**.
- **Ước lượng có căn cứ**: throughput đơn luồng **2,5x–4x**; với dump thực tế (15%
  script trùng) **3x–4,5x**; độ trễ một file lớn **4x–8x** trên máy nhiều core
  (hiện bị chặn bởi phần chạy nối tiếp). **10x throughput đơn luồng không có căn cứ**
  nếu giữ nguyên các thuật toán tìm kiếm tốn kém (de-inline, reconstruction search,
  naming); 10x chỉ khả dĩ cho độ trễ file lớn trên nhiều core.
- Chi phí: lớn nhất từ trước tới giờ (~115k dòng Rust liên quan), làm theo từng giai
  đoạn, mỗi giai đoạn phải cho output **giống hệt từng byte** trên 3.978 file và qua
  mọi gate hiện có. Có một mốc go/no-go sớm (§6).

## 2. Chi phí hiện tại (đo)

Binary release (fat LTO) có PDB, corpus 3.978 file (1,66 triệu instruction, 26.046
prototype), một luồng. Sampler tự viết (suspend thread + `StackWalk64`, không cần
quyền admin), 3 lượt, **3.426 mẫu đang chạy CPU**.

| Nhóm theo hàm lá | Tỉ lệ |
|---|---:|
| Logic (pass, SSA, cấu trúc) | 46,7% |
| Allocator (`mi_malloc_aligned` riêng 7,7%) | 13,5% |
| Visitor duyệt cây (`visit_local_reads`, `rvalues`, …) | 13,0% |
| Hash map/set (hashbrown, FxHasher) | 9,7% |
| I/O file (tạo/ghi/đổi tên từng file output) | 7,7% |
| Chuỗi/format | 5,0% |
| drop / clone / memcpy | 4,1% |

Khoảng **40% thời gian là hạ tầng biểu diễn**, chưa kể phần hash/cấp phát đã được
inline vào các hàm "logic".

Inclusive theo giai đoạn (chồng lấn, không cộng):

| Giai đoạn | Inclusive |
|---|---:|
| Pha SSA theo hàm (`decompile_function`) | 43,5% |
| · cleanup rounds / construct / inline / restructure / destruct | 12,0 / 10,1 / 8,3 / 7,7 / 7,4% |
| Các pass AST cấp chunk | ~48% |
| · UI rebuild / temp inline / naming / deinline / expr deinline / format | 5,6 / 5,5 / 5,1 / 3,8 / 2,8 / ~2,7% |
| Lifting | 2,5% |

Cấu trúc code liên quan: 475 collection khoá theo `RcLocal` (`Arc<Mutex<Local>>` +
id), hơn 1.700 lời gọi `.lock()`, 22 chỗ `Arc<Mutex<Block>>`, 56 chỗ logic dựa vào
`Arc::count`/`strong_count` (ngữ nghĩa ownership dính vào refcount, lý do W11–W15
không thể đổi biểu diễn một cách cơ học).

### Độ trễ một file lớn bị chặn bởi phần nối tiếp

| File | 1 luồng | 2 | 4 | 8 | 24 |
|---|---:|---:|---:|---:|---:|
| LightningCore (83 KB) | 115 ms | 90 | 77 | 78 | 82 |
| Write.lua (40 KB) | 79 ms | 77 | 77 | 78 | 78 |
| init.lua (34 KB) | 49 ms | 41 | 38 | 37 | 38 |

Chỉ pha SSA chạy song song theo hàm; các pass AST chạy một luồng trên cả file, nên
thêm core không giúp. Khởi động process ~10 ms (`--version` 9,7 ms).

### Việc lặp lại

586/3.936 file (15%) trùng byte-for-byte; 15,1% instruction nằm trong prototype
trùng. Folder mode không cache mặc định nên decompile lại toàn bộ.

## 3. PoC biểu diễn trên dữ liệu thật (đo)

Một probe tạm (không commit; mã lưu ở `D:\Medal\v22-work\ir_probe.rs`) chạy ở cuối
pipeline trên AST thật của mọi file, 5 vòng mỗi phép đo:

| Phép đo trên 2,19 triệu node | AST hiện tại | Arena phẳng | Nhanh hơn |
|---|---:|---:|---:|
| Census reads/writes/captures mỗi local (`collect_usage`) | 196,7–267,7 ms | 21,4–28,0 ms | **9,2–9,6x** |
| Deep copy cây (`dc_block`) | 134,7 ms | 5,7 ms | **23,4x** |

Hai bên cho kết quả census giống hệt (0 lệch trên mọi local). Arena ở đây là mức tối
thiểu (8 byte/node); node thật có thêm payload, nên hệ số thực tế cho deep copy nhiều
khả năng ~10x. Đây là giới hạn cho loại thao tác, không phải speedup toàn engine:
pass biến đổi có phần logic không nhanh lên theo biểu diễn.

## 4. Kiến trúc đề xuất cho V2.5

**K1. IR arena theo hàm.** `FunctionIr { exprs: Vec<Expr>, stmts: Vec<Stmt>,
blocks: Vec<BlockData>, operands: Vec<ExprId> }` với id `u32`; toán hạng là khoảng
trong một pool; chuỗi/tên intern ở bảng cấp chunk (`StrId`). Local là `LocalId(u32)`
dày; metadata local dạng SoA. Không `Arc<Mutex<_>>` bên trong một hàm; closure trỏ
`FunctionId`, thân hàm do chunk sở hữu và bất biến sau khi publish.

**K2. Fact store duy trì tăng dần.** Def/use list theo local, tập capture, tóm tắt
effect theo câu lệnh, được cập nhật bởi chính API biến đổi (`replace_expr`,
`remove_stmt`, `move_stmt`…). Các census (475 collection theo `RcLocal`) trở thành
truy vấn O(1) hoặc `Vec`/bitset theo `LocalId`. Thay 56 chỗ dùng `Arc::count` bằng
fact tường minh (số lần dùng, thân dùng chung).

**K3. SSA dày.** Giá trị SSA là `u32`, phi là `Vec`, liveness bằng bitset,
coalescing bằng union-find trên id dày (giữ nguyên thứ tự quyết định hiện tại, kể cả
sweep theo register của v2.2).

**K4. Pipeline song song toàn phần.** Pass theo hàm chạy song song (closure trong
trước khi cần); pass cấp chunk (deinline khớp chéo hàm, dành tên, rehoist) tách thành
pha map song song + pha reduce hợp nhất có thứ tự cố định để giữ determinism.

**K5. Không làm lại việc đã làm.** Folder mode dùng lại kết quả cho cặp (bytecode,
ngữ cảnh đặt tên) giống hệt; memo fact theo hash prototype trong một lần chạy.
Ghi file theo lô, ít syscall hơn, vẫn giữ tính nguyên tử hiện có.

## 5. Ước lượng (có giải thích)

| Thành phần | Tỉ lệ hiện tại | Sau thay đổi | Căn cứ |
|---|---:|---:|---|
| Hạ tầng biểu diễn | ~40% | ~5% | PoC 9–23x trên loại thao tác này |
| Logic pass + SSA | ~47% | 19–31% | fact duy trì bỏ census lặp; SSA dày; 1,5–2,5x là giả định |
| I/O | 7,7% | ~4% | ghi theo lô, bỏ file trùng |
| Chuỗi/format | 5% | 2–3% | tên intern, formatter trên arena |
| **Tổng một luồng** | 100% | **29–43%** | **2,3x–3,4x** |

Thêm dedupe trên dump thực tế (15% trùng) → **2,7x–4x**. Độ trễ file lớn: phần nối
tiếp giảm theo tốc độ lõi và được song song hoá theo hàm → **4x–8x** trên 8+ core.
10x throughput cần thêm thay đổi thuật toán cho các pha tìm kiếm (ví dụ de-inline
khớp bằng băm chuẩn hoá thay vì so từng cặp); chưa có số đo nào ủng hộ.

## 6. Kế hoạch an toàn

Oracle: output **giống hệt từng byte** với bản hiện tại trên 3.978 file, cộng toàn bộ
gate (1.315 test Rust, deep review 199, semantic 45, runtime v9/v12/v14, generated,
public, witnesses, bytecode oracles, compact style). Mọi khác biệt phải là cải thiện
được review, không bao giờ là "gần giống".

1. **Nền:** IR v3 + chuyển đổi AST↔IR + formatter trên IR. Gate: AST→IR→format giống
   hệt format hiện tại trên cả corpus.
2. **Mốc go/no-go:** chuyển pha SSA (construct/inline/destruct, 26–44% thời gian)
   sang IR dày. Nếu pha này không nhanh hơn ít nhất ~3x trên corpus thì dừng và báo
   lại, không đi tiếp.
3. Chuyển các cụm pass AST theo độ nóng (temp inline + UI rebuild, naming, deinline,
   cleanup/normalize…), mỗi cụm có cầu nối AST↔IR ở biên và gate giống hệt từng byte.
4. Song song hoá pipeline theo hàm; tách pass cấp chunk thành map/reduce.
5. Gỡ cầu nối và AST cũ khỏi đường nóng; dedupe + I/O theo lô.
6. Đo cuối cùng với cùng giao thức (nhiều cặp xen kẽ, CPU time, cùng toolchain).

Rủi ro chính: khối lượng rất lớn; ngữ nghĩa ownership (`Arc::count`) và publish thân
closure song song phải được mô hình hoá tường minh trước; các ngân sách tìm kiếm có
thứ tự (deinline, reconstruction) phải giữ đúng thứ tự quyết định. Cầu nối tạm thời
làm các giai đoạn giữa chậm hơn; lợi ích chỉ thấy rõ ở giai đoạn 5.

## 7. Cập nhật 2026-09-29: kết quả thực nghiệm

Đo trên nhân P cố định (máy 8 nhân P + 16 nhân E; một luồng không ghim dao động
tới ±50% tùy nhân được xếp lịch).

**Giả thuyết biểu diễn bị bác bỏ.** Hai thí nghiệm trực tiếp trên code thật:

| Thí nghiệm | Thay đổi |
|---|---:|
| Sao chép lại toàn bộ AST cho liền mạch bộ nhớ trước các pass cấp chunk | 0% |
| Bỏ khóa của `RcLocal` (ô không đồng bộ, chỉ để đo) | ~1% |
| mimalloc v2 / v3 / direct TLS | ±0% |

Hệ số 9–23x của PoC ở §3 đến từ việc so một census trên mảng sự kiện đã trích sẵn;
nó không đại diện cho pass thật. IR arena vì thế **không** cho 2,3–3,4x như ước
lượng ở §5 và kế hoạch §6 dừng ở mốc go/no-go. Chi phí nằm ở thuật toán: khoảng 40
pass cấp chunk (59% thời gian của chúng là lượt chạy không thay đổi gì) và pha SSA
theo hàm (inliner 12%, dựng SSA 11%, destruct 8%, structuring 9% phần tính toán).

**Đã làm, output giống hệt từng byte trên 3.978 file, 1.285 test Rust xanh:**
kiểm tra ứng viên trước khi tính census cho các pass hiếm khi thay đổi; folder mode
decompile một lần cho các file trùng; FxHash thay SipHash cho các map/set theo thứ
tự chèn; post-dominator và liveness (restructure, out-of-SSA) trên bit set dày;
bảng tra cứu SSA được định cỡ trước; khối lệnh thẳng được chuyển thay vì clone.

| Corpus, fat LTO | v2.2 | Hiện tại | |
|---|---:|---:|---:|
| 1 luồng | 11,50 s (CPU 9,16 s) | 9,14 s (CPU 7,34 s) | 1,26x |
| 24 luồng | 1,08 s | 0,94 s | 1,15x |

**Còn lại:** chế độ `--emit-upvalue-analysis` (Volt `decompile_all`) chậm hơn chế độ
thường 3–5 lần; riêng `FlushFileBuffers` từng file chiếm 5,5–8,9 s so với 3,2 s khi
bỏ ở 24 luồng. Bỏ fsync hoặc giữ handle thư mục là thay đổi ngữ nghĩa bền vững/bảo
mật, cần quyết định riêng. PGO cho thêm khoảng 5% (§W7) nếu có dữ liệu huấn luyện
trong CI. Các ý tưởng engine còn lại đều dưới 1–2% mỗi cái.
