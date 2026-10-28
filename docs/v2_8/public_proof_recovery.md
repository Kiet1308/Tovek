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
- [x] Hoàn thiện mốc checker có guard: 145 bytecode tests, 343 Python tests;
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
486 output + 27 typed refusals; 141 `proved`, 174 `different`, 171 `unknown`.
Toàn bộ 92 proofs cũ được giữ. Runtime replay vẫn đủ 294 inputs, gồm 290 output
+ 4 refusals, giữ 149/149 proofs cũ. Graph self-controls qua đủ 486 và 290.

Trong riêng 77 gaps: **43 proved, 22 different, 12 unknown**. Có 8/20
`different` và 35/57 `unknown` được chứng minh; 12 output table nil cũ vẫn
`different`, 10 ca FASTCALL chuyển từ `unknown` sang `different` vì model mới
giữ cả native và fallback. Đây là mốc checker, chưa phải kết quả của engine đã
sửa. Không tính thay đổi nhãn thành runtime pass.

Receipts: `public-proof/checker-namecall-final-on-baseline-{public,runtime}.json`,
`public-proof/review/final-review-receipt.{md,json}` dưới work root ở trên.
Full engine audit và replay 77 profiles trên output cuối vẫn đang thực hiện.
