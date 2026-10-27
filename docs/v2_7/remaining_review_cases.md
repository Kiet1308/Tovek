# Ba review cases chuyển backlog chờ dữ liệu

Ngày 28/10/2026, UTC+7. Code hiện tại `b798e5b` đã qua 1.698 workspace tests,
801 semantic, 469 deep-review và đủ 33 bước audit. Sau quyết định G1 riêng,
chủ repo đã chọn **“Chuyển 3 mục sang backlog chờ dữ liệu”** để chốt phần
decompiler V2.7 đã được kiểm chứng. Ba mục dưới đây không còn chặn goal này,
nhưng vẫn chưa được sửa/nghiệm thu về kỹ thuật.

| Mục trong roadmap | Bằng chứng khả dụng | Phần còn thiếu để nghiệm thu chính mục đó |
|---|---|---|
| S1: “Handle splits without case (c)” | [Audit implementation](audit_existing_m4_m6.md) kiểm tra các gate S1 và bảo toàn service/module handles D4 | Không có định nghĩa case (c), dạng input/output hoặc acceptance rule trong source, git history hay các plan khả dụng; không được đồng nhất nó với D4 |
| `repeat` với `return` trong `if`, O2 | Structurer đã có bounded shared-return handling; các public repeat/return và compiler-inlining probes đã qua | Chưa có source/bytecode gốc gây refusal, nên chưa xác định được đúng CFG/rejection mà review mô tả |
| Nested inlined calls trong generic `for`, O2/g2 | Differential SSA tests, semantic controls và public generic-for probes đã qua | Chưa có danh tính/source/bytecode của 3 seeds trong bộ 45.000 seeds gốc |

[Stress của checkpoint split prefix](stress_split_prefix.md) gồm 135.000
profiles không failed/refused, 9.000 mutation profiles không failed và hai bộ
512 probes đều qua. Trong 1.024 probes có 384 generic-for và 640 repeat/return
profiles; bộ repeat-inline có 496 compiler-inlining witnesses, 16 controls.
Các số này thuộc binary `8c5d3feb…`, không phải kết quả mới chạy trên `b798e5b`.
Coverage rộng không nhận diện được original input và không thay một đặc tả.

Đã chạy lại hai bộ review probes trên binary nghiệm thu `b798e5b`, SHA256
`f1f019b6564b9447774b622ffac023155fcef3c23bb7b4d3acfe13adf5b5b094`:
512/512 structuring và 512/512 repeat-inline đều qua, vẫn 496 inlining
witnesses. So trực tiếp 1.024 emitted outputs với `d3e64a2`: giữ từng byte.
Evidence tại `/workspace/work/v27/review-probes-phi-singleton-summary.json`,
`review-probes-phi-singleton.log`, hai thư mục `*-probes-phi-singleton` và
runner `run_review_probes_phi_singleton.py`.

Mỗi mục hiện giữ trạng thái **chờ dữ liệu/đặc tả**, không ghi “đã sửa”. Điều
kiện tiếp tục: mô tả before/after của case (c), hoặc một source/bytecode/seed
có thể dựng lại với compiler pin và O/debug flags tương ứng. Khi tái hiện
được, giảm ca bằng harness hiện hữu, sửa ở tầng proof/SSA/structurer phù hợp,
thêm fixture và kiểm chứng output/oracle theo workflow.

Theo quyết định của chủ repo, nghiệm thu phần decompiler đã triển khai bằng
public suites hiện có và giữ riêng ba mục này trong backlog. Không coi việc
thiếu dữ liệu là proof không còn bug; không thay đổi refusal policy để che
các ca chưa rõ. [Báo cáo nghiệm thu](ACCEPTANCE.md) ghi phạm vi và giới hạn.
