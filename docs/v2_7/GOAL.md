# Hoàn thiện decompiler Tovek V2.7

Trạng thái: đang triển khai; baseline workspace và semantic đã đạt, chưa nghiệm thu toàn bộ.
Ngày lập: 27/10/2026, UTC+7.
Nhánh thực hiện: `v2.7-dev`. Điểm xuất phát sau pull: `76212c9`.
Nguồn yêu cầu: [ROADMAP.md](ROADMAP.md), cập nhật ngày 27/10/2026, và yêu cầu của chủ repo hoàn thiện toàn bộ decompiler, trừ release và web changelog.

## Mục tiêu

Hoàn thiện toàn bộ hạng mục decompiler còn mở trong mục 1–5 của roadmap: hiệu năng, M4 structure, M5 naming, tồn đọng review V2.6 và M6. Giữ đúng hành vi bytecode, cải thiện khả năng khôi phục và độ dễ đọc, kiểm chứng mỗi thay đổi bằng fixture, semantic gates và số đo phù hợp. M6 nằm trong phạm vi đầy đủ của goal này, không bỏ qua vì nhãn “if time allows” trong roadmap gốc.

Goal kết thúc bằng code và bằng chứng nghiệm thu trên `v2.7-dev`. Không tự đóng mục còn thiếu kiểm chứng; trường hợp thiếu dữ liệu, thiếu quyết định hoặc chưa có cách triển khai đúng phải giữ trạng thái bị chặn, nêu rõ điều kiện tiếp tục.

## Phạm vi

- Trong phạm vi: engine, SSA/CFG/structurer, AST, deinline, naming, formatter, bytecode validation, CLI/API integration cần thiết, fixtures, công cụ kiểm chứng và benchmark decompiler.
- Ngoài phạm vi: bump version, package phát hành, tag/release, release notes, merge hoặc fast-forward `main`, deployment, website, web changelog, site data và làm mới module decompiler cho website.
- Vẫn chạy kiểm tra tương thích Wasm/Worker có trong CI khi engine dùng chung yêu cầu; không phát hành hoặc triển khai chúng.
- Giữ benchmark cuối bằng release build so với V2.6.1 và V2.5.1: đây là nghiệm thu engine, không phải hành động phát hành.

## Nền hiện có

Giữ năm thay đổi đã có: helper với merged result (`aa3f767`), cleanup control flow (`c864e16`), capture census có điều kiện (`125d12a`), vector spelling Roblox (`0da3755`), live self-updates (`1aeabd8`).

Roadmap ghi nhận tại `1aeabd8`: 1.636 workspace tests, 699/699 semantic round-trips, local CI 35/35. Đây là kết quả của lần chạy trước, không thay thế kiểm chứng bản triển khai mới. Không thực hiện lại các tính năng đã hoàn thành chỉ để tạo diff.

## Các quyết định phải giữ

- D1: không giả định thư viện chuẩn chưa bị thay thế; không dùng builtin evaluation/folding để chứng minh match; không thêm tier-1 spelling bundle, kể cả qua flag.
- D2: mọi rebuilt call giữ comment tại site, dù có thể rút ngắn câu chữ.
- D3: chỉ dùng if-expression khi rõ hơn, hai nhánh ngắn ở vị trí giá trị; còn lại giữ statement.
- D4: `GetService` và `require` giữ handle là named locals. Chỉ fold đường `GetService` trùng và constructor/template slots đúng điều kiện.
- Khác vị trí dòng trong thông báo `error(msg, level)` không chặn rebuild helper.
- Evidence naming ưu tiên hơn type fallback. Suffix theo scope; không dùng bộ đếm toàn file, tên gây shadow, tên lớp suy từ `__index`, AI hoặc ML naming.
- Tối ưu hiệu năng phải giữ output byte-identical với baseline ngay trước bước tối ưu. Không tắt quality pass, giảm proof budget hay đổi quyết định tìm kiếm để làm benchmark nhanh hơn.
- Giữ capture/closure identity, evaluation order, multret, NaN, signed zero, metamethod, CLOSE/iteration cells, lỗi và khả năng quan sát table layout.
- Đọc `docs/performance/dead_ends.md` trước thí nghiệm engine; ghi số đo và lý do của hướng bị loại.
- Áp dụng Ponytail `full`: đọc đủ luồng dữ liệu, tái sử dụng cơ chế sẵn có, sửa nhỏ nhưng hoàn chỉnh, không rút bớt validation hoặc proof để giảm code.

## Thứ tự thực hiện và tiêu chí từng mốc

### G0 Chuẩn bị baseline

- [x] Xác nhận working tree và nhánh; giữ các thay đổi của người dùng.
- [x] Thiết lập Rust `nightly-2026-06-15`, Luau pin `c2ec0d4e5ca50796ba174a7565298f59aa572268` và benchmark VM.
- [x] Build baseline từ `76212c9`, lưu binary và provenance tại `/workspace/work/v27`; snapshot semantic fixtures trước khi sửa. SHA256 binary: `915fecbf7911a83abeeb50b0a292a1a5d133b6c79293cf0e9c711fc86f305e3d`.
- [ ] Chạy baseline gates và phân biệt lỗi sẵn có, lỗi môi trường và lỗi mới.
- [x] Chủ repo xác nhận không có nguồn truy cập bốn frozen dumps. Tiếp tục implementation và public validation; không tuyên bố đã nghiệm thu trên 9.987 input riêng.
- [ ] Đọc các plan/review local nếu có. Tách các chi tiết chưa đủ định nghĩa trong roadmap, đặc biệt “handle splits without case (c)”, thành dependency cần làm rõ; không tự đoán phép biến đổi.

### G1 Hiệu năng

- [ ] Profile lại chi phí thật; tách profiling khỏi benchmark release không instrument.
- [ ] Ưu tiên `D_SCAN` và `D_ROUND_CENSUS`: census theo hàm với invalidation đúng phạm vi, hoặc gate vòng không thể admit; chứng minh không bỏ cơ hội match mới sau splice trong block con.
- [ ] Sau đó xử lý `D_COLLECT_TARGETS`, `S_FORMAT`, `DESER_LIFT`, `DESER_INLINED_COPIES`; chỉ mở rộng sang naming hoặc literal-sharing khi profile mới cho thấy cần thiết.
- [ ] Mỗi candidate giữ output từng byte, thứ tự rewrite/candidate và kết quả proof; đo A/B release cùng toolchain, allocator, flags, input và worker count.
- [ ] Mục tiêu là khắc phục regression so với V2.5.1 ở một luồng, kiểm tra cả đa luồng, đồng thời so V2.6.1. Lưu toàn bộ sample và report workload riêng; không suy speedup từ phase timing hoặc synthetic operation counts.
- [ ] Nếu chưa khắc phục được regression, báo rõ còn chưa đạt; không lấy việc thử nhiều phương án làm tiêu chí hoàn thành hiệu năng.

### G2 M4 Structure

- [ ] S1: guard-continue gate; giới hạn tail duplication; terminal if/else theo weight; kiểm tra scope của `factor_common_tails`.
- [ ] S1: `negate_exact` và guard split không đảo toán tử quan hệ khi có NaN; truthiness collapse; chuỗi and/or chỉ dùng truthiness của literal và constructor theo D1.
- [ ] S1: comparison orientation ở SSA inliner; snapshot-noise refinement; colon precision; index tuple và swap folds.
- [ ] S1: statement residue từ chối khi `if` nằm trong vòng lặp không chứa declaration; handle splits loại case (c), sau khi xác định được đặc tả của case này.
- [x] S4: if-expression với hai atomic arms, tối đa 72 cột, chỉ ở vị trí giá trị.
- [ ] S4: placeholder `0` chỉ đổi thành `nil` khi không thể đọc table trước lần store đầu vào key đó.
- [x] Khôi phục header `while isAlive(h) do` từ rebuilt call; dùng `open_cases/while_inlined_condition.luau`, bảo toàn số lần và thứ tự đánh giá điều kiện, break/continue và scope.
- [ ] L3: đo golden diff của EQ-jump polarity/branch order trước khi chọn triển khai. Nếu không nhận thay đổi, ghi quyết định dựa trên diff và semantic/readability evidence, đúng tính chất đánh giá của mục này.

### G3 M5 Naming

- [ ] N1: `NameTier`, override matrix, consumer evidence và veto cho tên gây hiểu sai.
- [x] Sửa ca `CharacterAdded` nhận tên `parent` trong `open_cases/event_param_named_from_parent_store.luau`: event evidence thắng suy luận từ store `.Parent`.
- [ ] N2: Roblox API tables có version, chỉ cung cấp tên; literal `.Name = "X"`; module/class/self roles, giữ B3 của `recover_methods`.
- [ ] N2: phục hồi discarded binding chỉ khi còn register/local headroom.
- [ ] N3: unanimous phi votes; tuple roles; irregular plurals; type fallback yếu dưới mọi evidence rule, suffix theo scope.
- [x] Hoàn thiện type-digit naming `vector2`/`vector3`, phối hợp reservation S3; không shadow tên global cần in ra.

### G4 Tồn đọng review V2.6

- [ ] `repeat` có `return` trong `if` ở O2: path walker sao chép terminal block nhỏ đúng scope/capture; chuyển refusal thành output đúng cho các ca được chứng minh.
- [ ] Nested inlined calls trong generic `for` ở O2/g2: tìm reproducer cho lỗi fuzz, sửa debug-local boundary/phi hoặc edge-copy placement trong SSA; không che lỗi ở formatter.
- [ ] Chuyển panic do malformed bytecode thành typed refusal tại `bytecode_validate`: `unreachable LOP_BREAK`, `unwrap(None)`, “not implemented”, FORGPREP thiếu partner. Giữ catch_unwind làm lớp bảo vệ cuối.
- [ ] Mô hình NEWTABLE: size hint khác stores được fold; record value nil tại runtime. Kiểm tra thứ tự duyệt/khả năng quan sát; không tự bracket hàng loạt constructor khi chưa đo tác động.
- [x] Xây exact vector fold model trước khi mở E2 (`ast/src/deinline/fold.rs`) cho helper nhận vector constants, như `reflect(v, Vector3.new(...))`; không dùng giả định thư viện chuẩn bất biến.
- [x] Chủ repo chọn giữ chính sách line census hiện tại cho structural rebuild trên size floor. Không thêm gate làm mất rebuilt calls.

### G5 M6

- [ ] L1: blank lines theo source lines; `SourceLines` không tham gia `PartialEq`.
- [ ] L2: return types trên final AST, chỉ những type đã chứng minh; không đưa annotation từ bảng tên API Roblox vào output.
- [ ] L4: hợp nhất deinline engine, expression mode thay legacy family và incremental helper cache. Di chuyển proof/capture/effect/arity/frame guards đầy đủ; giữ candidate order, ambiguity và budget; dùng differential reference trước khi bỏ đường cũ.
- [ ] Giữ output byte-identical cho phần cache/refactor thuần hiệu năng; tách thay đổi presentation có chủ đích để review và baseline riêng.

### G6 Nghiệm thu toàn bộ decompiler

- [ ] Mọi mục được triển khai và kiểm chứng, hoặc có quyết định cụ thể được chấp nhận cho mục vốn yêu cầu đánh giá/chính sách; không còn task bị bỏ qua vì thiếu thời gian.
- [ ] Workspace tests, toàn bộ Fixture and Luau audit và các check tương thích liên quan đều đạt trên đúng commit cuối.
- [ ] Hai open cases đã thành regression fixtures và có kết quả before/after rõ ràng.
- [ ] Mọi edge case mới nằm trong `docs/failure_fixtures/semantic_roundtrip`, kèm oracle và output-size baseline trong cùng commit code.
- [ ] Đọc diff bốn dumps before/after: không mất rebuilt calls hoặc tăng tổng dòng so với baseline phù hợp của từng bước; mọi delta có chủ đích được giải thích. Không tự nâng baseline để che regression.
- [ ] Benchmark cuối bằng release build so V2.6.1, V2.5.1 và điểm xuất phát V2.7, tách một luồng/đa luồng, public/private, latency/throughput và instrumented/ordinary.
- [ ] Cập nhật roadmap với commit, kết quả check, số đo và phần bị chặn thật. Bàn giao source, fixtures, baseline và báo cáo engine; dừng trước release, website và merge main.

## Quy trình kiểm chứng

Trước mỗi commit chạy `cargo +nightly-2026-06-15 test --workspace --all-targets -j 4` và mọi suite của job “Fixture and Luau audit” trong `.github/workflows/ci.yaml`. Workflow là nguồn lệnh và dependency chính xác; giữ thứ tự lineage trước binding graph và profile. Có thể dùng test tập trung trong lúc sửa, nhưng không thay chúng cho full gates trước commit.

Phải giữ semantic round-trip O0/O1/O2, deep review, fixed-seed fuzz, strict residual-control checks, output-size, runtime/determinism v9/v12/v14, compact style, public-source/compiler witnesses, capture/binding/metadata/cache controls và bytecode oracle theo workflow. Không coi `nonequiv` của bounded oracle là bằng chứng runtime sai hoặc là chứng minh tương đương đầy đủ.

Mỗi output change cập nhật `docs/bytecode_roundtrip/baseline_semantic_roundtrip.json` và `docs/output_size/baseline_fixtures.json` sau khi xem delta; không nới gate. Tách commit hiệu năng byte-identical khỏi commit readability/naming. Stage explicit paths; không stage `.lua` corpus, `docs/tess.c`, `luac.out`, private dumps hoặc raw private outputs. Chỉ force-add `.md`/`.json` thực sự muốn công bố.

Không chạy build, CI và profiling/benchmark cùng lúc. Giữ CRLF/LF của file khi sửa. Dùng corpus path ASCII nếu công cụ Luau không đọc được đường dẫn có ký tự ngoài ASCII.

## Dữ liệu và quyết định còn thiếu

- Chủ repo xác nhận không có bốn frozen dumps 9.987 input trong môi trường có thể truy cập. Public suites là phạm vi kiểm chứng khả dụng; kết quả không bao gồm private corpus hoặc timing trên máy Windows gốc.
- Chưa có các plan local `PLAN_V2_7_PERFORMANCE.md`, `PLAN_V2_7_READABILITY.md`, `2026-10-12-deinline.md`, bản thiết kế M0–M6 và các `fr-*.md`. Tìm lại khi cần các tiêu chí chưa được định nghĩa đủ trong handoff.
- Cần reproducer/seed cụ thể của lỗi generic-for và danh tính cụ thể của các malformed-bytecode panic; trước hết thử giảm ca bằng harness hiện có, không bịa dữ liệu từ bộ 45.000 seeds.
- Chính sách line census đã chốt: giữ hiện tại. D1–D4 và các binding decisions khác đã chốt, không hỏi lại.
- Có thể triển khai và kiểm chứng các mục độc lập trong lúc thiếu dữ liệu; không đánh dấu nghiệm thu toàn bộ cho tới khi các blocker cần thiết được giải quyết.

## Môi trường đã kiểm tra

Rust nightly đúng phiên bản có tại `/workspace/.setup`; shell hiện chưa đưa nó vào PATH. Có thể gọi lệnh với biến riêng cho command:

```bash
export CARGO_HOME=/workspace/.setup/cargo
export RUSTUP_HOME=/workspace/.setup/rustup
export PATH="$CARGO_HOME/bin:$PATH"
```

Luau source tại `out/luau-source` đúng pin CI; `out/luau-build/{luau,luau-compile,luau-ast}` và `out/benchmark-vm/benchmark-vm` đã tồn tại. Baseline mới: 1.633 passed, 2 ignored theo tổng dòng suite result của workspace; semantic 699/699. Test tập trung hiện tại: cleanup_control_flow 10/10, luau-lifter lib 103/103. Các số này chưa thay thế full gates sau cùng.

## Dọn dung lượng bằng Python

Yêu cầu bắt buộc của chủ repo: tuyệt đối không dùng lệnh `rm` để dọn cache hoặc dữ liệu khi thiếu dung lượng. Viết và dùng script Python, in đúng đường dẫn và xem trước phạm vi rồi mới xóa bằng Python. Không thay bằng `cargo clean`, `git clean`, `find -delete` hoặc lệnh shell xóa trực tiếp khác.

Script hỗ trợ hiện có: `/workspace/work/cleanup_build_cache.py`. Nó chỉ cho phép `target/{debug,release,fastrel}/incremental`, mặc định chỉ preview, từ chối symlink ở đường dẫn cache và giữ source/binary/fixtures/evidence.

```bash
python3 /workspace/work/cleanup_build_cache.py --profile debug
python3 /workspace/work/cleanup_build_cache.py --profile debug --apply
```

Chỉ chạy `--apply` khi cần và sau khi dừng build. Nếu cần dọn vùng khác, mở rộng allowlist Python bằng đường dẫn cụ thể sau khi kiểm tra ownership và dữ liệu cần giữ; không nhận đường dẫn xóa tùy ý, không xóa cả workspace, corpus, toolchain hoặc evidence chưa được bảo toàn. Hiện chưa cần dọn dung lượng.

## Checkpoint control flow / naming

Xem [báo cáo kiểm chứng](checkpoint_control_naming.md). Mốc này có 1.652 workspace tests passed, 711/711 semantic cases và toàn bộ Fixture and Luau audit. Exact vector fold đã có; reflect expression matching và các mục còn mở chưa được đóng. Chủ repo yêu cầu commit/push từng mốc đã kiểm chứng lên `v2.7-dev`.

## Checkpoint vector arguments

[Proof và kết quả](checkpoint_vector_arguments.md): hai call `reflect` từ vector constants đã khôi phục; workspace 1.655 passed, audit đủ 33 bước, public outputs byte-identical với `927885c`. Các mục khác của goal vẫn mở.

## Checkpoint copy captures

[Regression seed 7388 và kết quả kiểm chứng](checkpoint_copy_capture.md): sửa call nhầm snapshot khi SSA tái dùng local. Workspace 1.658 passed, 714 semantic cases, 301 deep-review cases và đủ 33 audit steps; 513 public outputs byte-identical với `0007889`. Proof bảo thủ giữ inline hai site cũ thay vì dựng call; diff được ghi rõ trong báo cáo. M6 và các mục mở khác tiếp tục.
