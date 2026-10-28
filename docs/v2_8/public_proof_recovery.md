# V2.8: nghiên cứu và xử lý 77 public proof gaps

Goal mở ngày 28/10/2026 (UTC+7), tiếp tục code `d6ffc29`, checkpoint
`dd791a9`. Chủ repo yêu cầu thực hiện cả hai nhóm: 20 `different` và 57
`unknown`. Phạm vi gồm sửa lỗi decompiler được tái hiện và mở rộng static
proof có guard, tests và evidence. G1, release và web changelog không thuộc goal.

## Baseline đã khóa

77 identities trong [manifest](../failure_fixtures/public_proof_gaps.json)
khớp source/input/output hashes của audit-7. Chạy lại checker cũ trên bytecode
đã ghim tái hiện đủ 20 `different` + 57 `unknown`. Binary baseline SHA-256
`b08e65c4a4521b6b99b991fbcdac22638f8e3fa8a5b8d61c539c7f88dbb46e83`;
Luau pin `c2ec0d4e5ca50796ba174a7565298f59aa572268`, g1/`--fflags=false`.

| Nhóm ban đầu | Profiles |
|---|---:|
| Explicit capture reads / branches | 8 |
| Private table allocation / nil-capacity expansion | 12 |
| FASTCALL | 31 |
| Loop preparation | 14 |
| Reference/upvalue lifetime | 11 |
| Recursive binding | 1 |

Các nhãn là điểm model dừng đầu tiên, chưa phải root cause của mọi profile.
`different` là cây symbolic khác nhau, không tự chứng minh lỗi runtime;
`unknown` không được đổi thành pass chỉ bằng bỏ qua instruction/effect.

## Tiêu chí hoàn tất

- Giữ đủ 77 identities và ghi kết luận từng profile; không bỏ các ca khó khỏi mẫu.
- Tái hiện lỗi engine bằng input bytecode và driver xác định, sửa tại tầng chung;
  các positive/negative controls phải giữ builtin identity, capture/lifetime,
  thứ tự side effects, arity và table layout.
- Proof mới phải bounded và từ chối context không đủ dữ liệu; phân biệt rõ
  static certificate, bounded runtime result và giới hạn chưa được chứng minh.
- Kiểm lại toàn bộ public corpus, semantic/matrix và các consumers liên quan;
  không nới ngưỡng hoặc cập nhật baseline để che regression.
- Review độc lập và commit/push từng mốc đã kiểm chứng lên `v2.8-dev`.

## Tiến độ

- [x] Fetch/đối chiếu remote, khóa 77 inputs và snapshot checker/binary baseline.
- [x] Chia nghiên cứu độc lập theo bốn nhóm; có reviewer chuyên kiểm proof.
- [x] Hoàn thiện mốc checker có guard: 156 bytecode tests, 354 Python tests;
  replay đủ public/runtime và giữ 92/92 cùng 149/149 proofs đã có.
- [ ] Sửa khác biệt FASTCALL thật đã tái hiện khi host thay `typeof` trong safeenv.
- [ ] Hoàn thiện proof capture-read/branch và private-table cases.
- [ ] Hoàn thiện proof cho loop, forwarded captures, reference lifetime/recursion.
- [ ] Kiểm lại 77 profiles trên output cuối và public corpus đầy đủ.
- [ ] Chạy gates liên quan, review output/metadata và ghi giới hạn còn lại.

Evidence đang thu thập tại `/workspace/work/v28/public-proof/`; bản cũ tại
`/workspace/work/v28/eager-key/final-public-proof-loss/`. Không có Roblox runtime
cho toàn public corpus hoặc private frozen dumps trong môi trường này.

## Mốc checker, trước output engine mới

[Contract và các guard](proof_validator.md) mô tả từng certificate cùng các
false proof được sửa. Trên **output b08 đã khóa**, public vẫn đủ 513 inputs:
486 output + 27 typed refusals; 142 `proved`, 174 `different`, 170 `unknown`.
Toàn bộ 92 proofs cũ được giữ. Runtime replay vẫn đủ 294 inputs, gồm 290 output
+ 4 refusals, giữ 149/149 proofs cũ. Graph self-controls qua đủ 486 và 290.

Trong riêng 77 gaps: **44 proved, 22 different, 11 unknown**. Có 8/20
`different` và 36/57 `unknown` được chứng minh; 12 output table nil cũ vẫn
`different`, 10 ca FASTCALL chuyển từ `unknown` sang `different` vì model mới
giữ cả native và fallback. Đây là mốc checker, chưa phải kết quả của engine đã
sửa. Profile recursive 76 được chứng minh bằng certificate closed-read riêng;
mutant đọc sai upvalue vẫn bị từ chối. Không tính thay đổi nhãn thành runtime pass.

Receipts: `public-proof/checker-closed-read-final-on-baseline-{public,runtime}.json`,
`public-proof/review/final-review-receipt-closed-read.{md,json}` dưới work root ở trên.
## Mốc engine: GC placeholders và scalar native calls

Binary `552f5553090429b9d9e895f785e0b90e196dea9e98503198598865f232a618e2`
sửa hai lỗi runtime đã tái hiện: nil placeholder của DUPTABLE có thể bị GC
biến thành dead key, và việc group callee làm mất FASTCALL khi host thay builtin
trong safeenv. Giữ loader zero khi có thể collect; giữ actual scalar native call
bằng mutable argument binding có guard về capture, shadowing và register budget.
Các consumer metadata kiểm tra đúng producer/model và không coi local tổng hợp
là input ancestry.

Replay đủ 513 public inputs: 486 outputs + 27 refusals giữ nguyên; checker v3
cho **165 proved, 151 different, 170 unknown**, giữ 92/92 proofs cũ. Riêng
77 gaps là **60 proved, 6 different, 11 unknown**: toàn bộ 20 `different`
ban đầu và 40/57 `unknown` đã được chứng minh. Cả 40 annotations cùng bốn
helper sites của checkpoint trước được giữ. Không đếm runtime pass thay proof.

Kiểm chứng mốc này: 1.791 Rust tests (2 manual probes ignored), 355 Python
tests và đủ 33/33 fixture audit steps; semantic 846 = 811 passes + 35 refusals.
Ma trận 1.800 gồm 1.724 passes,
24 refusals và 52 compiler-sensitive profiles. Bốn compiler boundaries mới
được tái hiện nguyên vẹn trên binary b08 và source gốc; giữ riêng khỏi passes.
GC controls qua 54/54; metadata qua 63 outputs ở 7 modes và 44 producer controls.
Native adversarial suite có 763/837 passes; 54 source-compiler boundaries và
20 failures đều giữ nguyên observations của b08, không phải passes mới.

Legacy bytecode-signature gate báo 20 per-file increases; review thêm một ca
tăng so với b08 nhưng đã nằm trong threshold cũ. Cả 21 ca chỉ thêm mutable
argument barriers/native spelling, giữ nguyên runtime; bốn fixture mới đều
pass trên binary mới và fail trên b08. Comparator, refusal inventory và capture
diagnostics không bị nới. Kết quả gate này không phải static certificate.
Baseline chỉ nhận 20 increases và bốn fixture mới đã review, đồng thời siết 16
entries đã cải thiện; tổng nonequiv đúng 528, bỏ 23 điểm headroom cũ.

Receipts dưới work root: `public-final-v3.json`, `native-acceptance-v8/report.json`,
`differences/final-{gc-552f,native-metadata-552f-fixed,20-552f}/results.json`,
`legacy-oracle-review/baseline-update-recommendation.json` và
`../audit-8/results.json`. Các lượt thiếu dung lượng được giữ riêng và chạy lại;
evidence cũ được nén lossless, kiểm tra hash trước khi Python xóa bản chưa nén.

Tiếp theo: sáu C2 sites còn lại cần argument barriers tại đúng declaration site;
bốn profile `unknown` khác còn mất native dispatch. Các thử nghiệm source đã
được review độc lập, nhưng chưa tính vào kết quả engine mốc này. Cả 11 unknown
đã có per-profile disposition tại `loops/remaining-unknown/dispositions.json`;
không bỏ guards REF/open-pack/CFG chỉ để nâng trạng thái proof.
