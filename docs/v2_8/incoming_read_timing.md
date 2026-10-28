# V2.8: thời điểm đọc, thứ tự gọi và kiểm chứng bytecode

**Trạng thái: hoàn tất kiểm chứng và review ngày 28/10/2026 UTC.** Code
`6f149118857532779c05f9660cb3709bb3594be0` đã commit/push lên `v2.8-dev`. Checkpoint
này tiếp tục [owned-key recovery](owned_key_timing.md) trên `v2.8-dev`, trong
phạm vi decompiler và verification. Không release, đổi version, tag, merge
main, làm web changelog hoặc mở lại G1.

Baseline đầu đợt là code `302f9a3`, binary SHA-256
`bf265a3363380835c5258f6e0e66bd7fa3a86a40e5c99d2b9cee4b7de6ffdcdc`.
Binary cuối đã ghim trong `binary-final-release.json`, SHA-256
`778005572654c3edce6d243bfb99cc1a2d31692b2e36ffd4128e9ab80719bd8e`.
Phân tích output ban đầu dùng candidate `4453f5f3…`, SHA-256 đầy đủ
`4453f5f3b14022bcd0d8cade1d49a516b109ab480f3bd44435e4ebdbda0a9133`,
tại `binary-quality-release.json` và `audit-5`. Các kết quả bên dưới đã được
chạy lại trên binary cuối; các bằng chứng trung gian vẫn được giữ.

## Các lỗi và cách sửa

Incoming upvalue được GETUPVAL đọc trước callback. Khi compiler O2 inline
helper của output, nó có thể biến upvalue thành register của hàm ngoài rồi
đọc register sau callback. Saved copies bị loại bỏ ở input g0/g1 cũng gặp
vấn đề này. Thay đổi bảo toàn lần đọc sớm ở constructor keys, indexed
stores, arithmetic/comparisons, indexing, NAMECALL receivers và các nhóm
arguments của CALL/FASTCALL, kể cả builtin aliases. Địa chỉ assignment còn
được kiểm tra với các phép ghi local đứng trước nó trong cùng assignment.

Incoming proof yêu cầu census hoàn chỉnh, Function identity đã biết và tập
capture IDs nhất quán ở mọi occurrence. Union membership riêng lẻ không đủ
để kết luận. Copy/Ref vẫn được giữ nguyên; ownership proof hiện hữu không
đổi. Open-list recovery snapshot incoming key trước value effects, còn
register key của chính hàm giữ thời điểm đọc sau value effects. Allocation,
native SETLIST growth và thứ tự lỗi nil/NaN tiếp tục qua các proof cũ.

Pass cuối dùng `x or x` ngay tại vị trí đọc cần snapshot, sau expression
cleanup và trước final register checks. Nó không thêm call, frame, local
declaration hoặc hoist qua field/nhánh khác. Simplification cũng giữ
`x or x` và `x and x` khi hai operands là cùng bare local: đổi chúng về `x`
có thể làm mất snapshot đã có trong bytecode. Các phép giảm literal và
non-local không thuộc thay đổi này.

FASTCALL fallback có thể lấy callee sau arguments; ordinary CALL và callee
đã được lưu vào local có thứ tự khác. Không thấy chunk ghi đè thư viện chưa
chứng minh lookup không có hiệu ứng: môi trường ngoài vẫn có thể thay bảng
thư viện hoặc `__index`. Shared motion guards ở SSA, import folding và
helper reconstruction nay giữ lookup, captured reads/writes và argument
effects đúng thứ tự. Import folding chỉ đi tới statement thực thi liền
kề sau comments/empty statements và vẫn cần proof single-use, writes,
closure và dynamic environment.

Một runtime call như `math.max(1, 2)` có thể bị compiler của output gấp hằng,
làm mất lời gọi replacement từ môi trường ngoài. Các call đủ proof được
in thành `(math.max)(...)`; saved callees và readonly captures chỉ được nhận
khi census chứng minh việc đổi spelling giữ thứ tự. Native vector
constructor được nhận bằng FASTCALL builtin ID 54, không dựa vào tên
`Vector3.new` hay `vector.create`. Flag native và flag grouped callee được
giữ qua copies, equality và mọi structural matcher. Điều này sửa cả lỗi
gộp hai nhánh native/ordinary vector khác nghĩa dù input/output cùng O1.

Open CALL results đang chờ consumer mà phải đi qua lookup/operation khác
chưa có tuple-order proof được từ chối riêng sau structural validation:
`cannot preserve open call results across an operation without a tuple-order proof`.
Đây là bytecode hợp lệ chưa được xuất an toàn. Không thay bằng scalar
snapshot làm mất arity, tuple packing hoặc wrapper thêm frame. Adjacent
native consumers và những trường hợp đã có proof vẫn được nhận.

## Recovery và chất lượng output

Census riêng cho read timing duyệt expression bằng work stack, không dùng
giới hạn độ sâu expression hoặc tổng byte string literal của helper
planning. Nó vẫn có giới hạn 200.000 nodes và 128 tầng control/closure;
census cũ giữ nguyên contract. Nhờ đó expression 200 terms và literal trên
8 MiB không còn tự làm mất các capture facts cần thiết. Hazard còn mơ hồ
hoặc hết node/control budget vẫn bị từ chối.

Review output bắt được mất 220 inferred calls trong
`unfoldable_temporaries_over_local_limit__O2`: helper giữ CONCAT snapshot
trước lookup, còn copies dùng literal arguments nên có shape khác. Recovery
mới chỉ chuẩn hóa pattern đã được chuyên biệt hóa với primitive
string/number đủ proof, trong statement liền kề; không di chuyển CONCAT
của giá trị chưa biết, không giả định metamethod vô hiệu ứng. Probe riêng
khôi phục đủ **220 sites**, và output O0/O1/O2 cùng khớp bytecode input O2.
Việc chỉnh pattern giữ nguyên shared Function bodies.

Snapshot của captured local chỉ đọc còn được chuẩn hóa trước khi cache
helper patterns khi census đầy đủ và Function bodies độc nhất. Guard giữ
single-write/single-read, destination không bị capture, source không bị
gán lại, explicit binding và orphan constraints. Các probe
`loop_return_flag` và `deinline_merged_result_branches` giữ đủ năm inferred
calls mỗi fixture. Cùng CONCAT fixture, cả chín phép so input O2 với output
O0/O1/O2 trên release cuối khớp nguyên vẹn exit/stdout/stderr.

Saved iterator/API callees nay vẫn cung cấp naming hints mà không cần gộp
lookup trở lại expression. Các tên `child`, `descendant`, collection
elements và argument roles của `buffer` được giữ qua alias trực tiếp đã
chứng minh ổn định. Reassignment, captured writes, namespace bị shadow và
alias chains không được dùng để đoán tên. Naming không di chuyển call hay
đổi capture identity.

Review cuối còn ghi nhận **4 sites chưa khôi phục helper** ngoài
220 CONCAT sites đã sửa: hai `refresh` sites trong
`deinline_closure_factories__O2`, một `isCallable` site trong
`deinline_returns_distributed_and_short_circuit__O2`, và một `show` site
trong `exact_fraction_constants__O2`. Snapshot/guard layouts chưa khớp
template nên giữ phần code mở rộng. Các fixture đó qua same-profile
runtime trên binary cuối; đây là giới hạn recovery, không phải claim output
giống baseline hoặc helper đã khôi phục.

## Oracle runtime và evidence cho refusals

Semantic harness cũ compile input ở O0/O1/O2 nhưng chạy lại source gốc
bằng CLI mặc định O1. Fixture incoming O2 đã chứng minh phép so này có thể
báo sai oracle. Harness nay chạy **đúng bytecode input** và bytecode rebuilt
qua benchmark VM, cùng optimization và g1/`--fflags=false`; vẫn compile
output O0 để kiểm tra cú pháp. Report ghi `output_optimization` và raw
exit/stdout/stderr, giữ cả whitespace và byte UTF-8 không hợp lệ. Ma trận
riêng tiếp tục kiểm tra đủ chín tổ hợp; không đổi output optimization theo
source family để làm ca lỗi qua.

Ba fixtures dùng `loadstring` chỉ để khởi tạo global đã chuyển bootstrap
sang `prelude/<source>.luau`. Prelude chạy riêng trước input/rebuilt,
compile cố định O1/g1 và giữ unsafe-environment effect bằng `getfenv()`.
Không mở `loadstring` trong VM. Source/prelude hashes, flags và thứ tự chạy
được ghi vào report; preludes không được tính thành decompiler inputs.
Cả chín observations của source cũ chạy CLI, input mới với prelude và
rebuilt bytecode đã được đối chiếu bằng nhau. Chín output migrations này
được tách khỏi phép so output trên source input không đổi.

Refusal manifests khóa source hash, optimization/debug/version/options
và diagnostic cụ thể. Residual fixtures còn khóa toàn bộ bảy saved inputs,
wrapper/decoded-bytecode hashes, key 203, mode và prototype/pc. Mọi batch
phải khớp đủ inventory, từng diagnostic, exit, footer và output path;
panic, lỗi lạ, partial/empty/symlink outputs đều không được miễn trừ.
Output-size và bytecode-oracle consumers dùng lại validation này. Refusal
giữ mẫu số input và không tham gia runtime/equivalence totals; chuyển từ
baseline `ok` sang refusal vẫn cần review baseline rõ ràng.

## Ma trận và mức bao phủ đã quan sát

Cùng **93 cases × 9 tổ hợp × g0/g2 = 1.674 profiles**, baseline đầu đợt có
1.546 runtime passes, 70 failures và 58 compiler-sensitive profiles.
Binary cuối `77800557…` đã lặp lại kết quả của candidate `4453f5f3…`:
**1.608 runtime passes + 18 expected refusals + 48 compiler-sensitive
profiles; 0 failed** (`audit-6`, step 7).

| Chuyển trạng thái so baseline | Profiles |
|---|---:|
| Failure → runtime pass | 66 |
| Failure → expected refusal | 4 |
| Runtime pass → expected refusal | 14 |
| Compiler-sensitive → runtime pass | 10 |

Input identities và observations của toàn bộ 1.674 profiles giữ nguyên
trong phép so A/B. Đã bỏ đúng 10 sensitivity entries được chứng minh phục
hồi; 48 records còn lại giữ nguyên. Coverage loss 14 profiles được ghi rõ,
không tính thành improvement runtime.

Dữ liệu nghiệm thu trên binary cuối, `audit-6`:

| Suite | Kết quả | Binary |
|---|---|---|
| Default deep review | 606 = 577 runtime passes + 29 expected refusals | Cuối, step 7 |
| Semantic same-profile VM | 834 = 799 runtime passes + 35 expected refusals | Cuối, step 11 |
| Roadmap default/v12/v14/compact | Mỗi mode: 294 = 290 runtime passes + 4 expected refusals; 9 controls | Cuối, steps 13/15/16/17 |
| Public source | 513 = 486 emitted/recompiled + 27 expected refusals | Cuối, steps 34/36 |
| Residual default và strict | Mỗi mode: 7 = 5 emitted + 2 expected refusals | Cuối, steps 9/10 |

Public và residual corpus chưa được thực thi trong Roblox. Kết quả
emitted/recompiled, parse, metadata hoặc static analysis không phải runtime
pass. Các bộ probes/stress hữu hạn cũng không chứng minh mọi bytecode.

Stress cuối trên binary `77800557…` không có failure: **9.000 profiles =
8.040 runtime passes + 879 refusals + 81 invalid**, và **3.000 profiles =
2.322 runtime passes + 549 refusals + 129 invalid**, trong đó 1.478 profiles
được mutation. Đây là fuzzer hữu hạn có điều chỉnh output optimization cho
một số họ source vốn nhạy compiler; nó được báo riêng với strict nine matrix.
Refusals và invalid inputs không được cộng vào runtime passes. `invalid` còn
bao gồm compiler/reference failures và giới hạn tài nguyên của probes;
không có nghĩa cả 210 input đều là malformed bytecode.

## 77 public profiles mất static proof

Trên binary cuối `77800557…`, 77 profiles trước đây có dataflow `proved` nay
gồm **57 `unknown` và 20 `different`**. Review giữ nguyên trạng thái này:

| Nhóm nguyên nhân quan sát được | Profiles |
|---|---:|
| FASTCALL chưa được static model hỗ trợ | 31 |
| Loop preparation chưa được static model hỗ trợ | 14 |
| Reference/upvalue lifetime chưa đủ proof | 11 |
| Recursive binding bị model báo read-before-definition | 1 |
| Explicit captured reads và branches làm khác symbolic tree | 8 |
| Thứ tự private table allocation/nil-capacity expansion làm khác tree | 12 |

Đã kiểm input/source identities, compile lại baseline/candidate, đọc cả
43 source patches khác nhau và các biến thể optimization. Prototype paths,
parameter/upvalue counts, ordered capture kinds và số CLOSEUPVALS sites
giữ nguyên; writable/self-capture proxy không tăng. Các chỉ số đó chưa chứng
minh đầy đủ reference-register mapping hoặc lifetime của 11 ca upvalue,
và không quan sát được toàn bộ writes qua closure khác. Table-allocation
timing còn ngoài contract của static model. Không đổi `unknown`/`different`
thành equivalence success, cũng không suy ra runtime đã kiểm chứng chỉ từ
review source hoặc metadata. Cả 77 trạng thái và diagnostics đã được tái
hiện trên binary cuối. Phép so riêng output `4453f5f3…` với output cuối đạt
`proved` ở cả 77 profiles, nhưng không cung cấp proof còn thiếu với input gốc.

So output trên input không đổi: 778 semantic profiles có 751 source changes,
5 residual outputs đều đổi và 486 public outputs có 444 source changes.
Chín semantic prelude migrations được tính riêng. Inferred-call annotations
trên phần semantic so được là 856 → 845; public giữ 40 và residual giữ 38,
không có thay đổi số annotation trên từng public/residual file. Ngoài bốn
sites còn bảo thủ, bảy sites mất recovery để giữ thứ tự lookup/captured reads,
hai sites chuyển sang reuse helper cấp cao hơn và hai sites mới được khôi phục.
Các phép đếm này phục vụ review độ dễ đọc, không phải semantic proof.

Output-size giữ nguyên toàn bộ 829 baseline entries cũ, chỉ thêm 12 inputs
mới: 804 outputs + 37 expected refusals, không có regression theo ngưỡng cũ.
Oracle baseline được cập nhật sau review raw bytecode, source/runtime deltas
và captures; comparator không đổi. Semantic oracle có 278 inputs gồm 268
outputs và 10 refusals; 1.684 prototypes gồm 842 exact, 336 equivalent, 449
differ, 40 missing và 17 extra. Residual có 5 outputs + 2 refusals, 137
prototypes gồm 79 exact, 19 equivalent và 39 differ. Không có file tăng
capture-excess proxy; `differ`/missing/extra vẫn chưa được chứng minh tương đương.

## Nghiệm thu trên binary cuối

| Hạng mục | Trạng thái |
|---|---|
| Rust workspace | 1.762 passed, 0 failed, 2 ignored; `workspace-final-summary.json` |
| Release SHA-256 và source provenance | Đã ghim `77800557…`; `binary-final-release.json` |
| Python suite | 253 passed; `audit-6/12.log` |
| Matrix và semantic VM | Đã qua steps 7/11, số liệu như trên |
| Stress 12.000 profiles | Đã hoàn tất, phân loại tại `stress-accepted-summary.json` |
| Audit-6 toàn bộ workflow, oracle và metadata | 33/33 steps, 34 records gồm public baseline; tất cả exit 0 |
| Output/annotation deltas và 77 static-proof losses trên binary cuối | Review độc lập hoàn tất; không thấy blocker trong phạm vi đã kiểm tra |
| Code commit/push | `6f149118857532779c05f9660cb3709bb3594be0` trên `v2.8-dev` |

Luau được ghim tại `c2ec0d4e5ca50796ba174a7565298f59aa572268`, Rust tại
`nightly-2026-06-15`. Chạy workspace `--all-targets`, native checks và
`phase-allocation-trace`; test/dev tắt debug symbols để đủ dung lượng nhưng
giữ assertions. Ordinary release profile không đổi. Audit dùng các lệnh
thật trong `.github/workflows/ci.yaml`; 34 records gồm 33 steps và public
baseline chạy lại độc lập.

Metadata trên 284 roadmap và 486 public scripts chung không mất event/count,
producer, omission hoặc call annotation. Có 212 roadmap và 16 public
sidecar-only deltas: toàn bộ chỉ dịch +12 dòng ở naming `rule_site.line`.
Review cuối và receipt hashes được khóa tại `final-review.md` và
`final-review-manifest.json`; không có source change sau binary đã kiểm chứng.

Evidence gốc tại `/workspace/work/v28/eager-key/`: `binary-final-release.json`,
`cross-baseline-proposal-matcher/release-verification.json`,
`semantic-prelude-review/comparison.json`, `residual-independent-controls/results.json`,
`iterator-naming-proof/semantic-annotation-deltas.json`,
`final-targeted-quality/runtime-matrix/results.json`,
`final-byte-comparison.json`, `stress-accepted-summary.json`,
`baseline-review-final/{release-oracle-review,applied}.json` và
`final-public-proof-loss/{results,classified,bridge-to-prior}.json`, cùng
`final-metadata-comparison.json` và `final-metadata-detail-review.json`.
`/workspace/work/v28/audit-5/` giữ reports của candidate tham chiếu;
`audit-6/` dành cho nghiệm thu cuối. Candidates bị bác và counterexamples
được giữ lại. Metadata lớn chỉ được nén lossless bằng Python sau kiểm SHA-256;
dọn cache dùng exact allowlist đã preview, không dùng lệnh `rm` trực tiếp.

## Giới hạn còn giữ

Constructor recovery vẫn giới hạn 32 fields, 64 regions, cửa sổ 64 statements
và motion fuel 4.096. Context mơ hồ, node/control budget hết hoặc tuple order
chưa được chứng minh tiếp tục bị từ chối. Chưa có source spelling tổng quát
giữ mọi native-vector bypass/fallback và multret qua mọi mức tối ưu.
48 sensitivity profiles còn lại gồm 22 closure identity, 16 table layout,
6 vector behavior và 4 caller-frame profiles; chúng không phải runtime passes.

Chính sách tương thích empty preallocated hash đã chốt không đổi;
`pairs`/`next` qua các mức tối ưu vẫn có giới hạn đã ghi nhận. Private dumps
và ba review cases V2.7 thiếu dữ liệu chưa được kiểm chứng. G1/performance,
release và website không thuộc checkpoint này.
