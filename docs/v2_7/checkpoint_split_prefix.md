# Khôi phục open SETLIST qua prefix bị tách

Baseline: `02173c3`. Code, tests và bytecode fixture phục vụ decompiler V2.7;
không thay version/release/site.

## Lỗi và thay đổi

Broad fuzz trên baseline có 133.803 passed, 1.094 invalid, 103 typed refusals
và không có failed trong 135.000 profiles. Cả 103 refusal thuộc 31 seeds đều
là open SETLIST: prefix chứa store lặp key hoặc store bị tách bởi temporary/
nhánh. SETLIST vẫn cần constructor native để giữ bulk resize của array.

Mở rộng chính pass snapshot hiện hữu: bắt đầu từ NEWTABLE riêng, kể cả prefix
rỗng, giữ thứ tự từng field store trước tail. Key/value được snapshot đúng vị
trí cũ; không gộp store nil vào giá trị cuối vì collision history có thể đổi.
Khi key có thể lỗi, giữ constructor/store kiểm tra ở chỗ cũ trước callback sau
đó; constructor cuối dùng các snapshot đã được kiểm tra. Không thêm frame hay
đọc/evaluate lại key/value.

Giữ mọi rào chắn: bảng chưa bị capture/đọc/escape; không vượt loop/control
transfer; cùng array-list index; fuel 4.096, 64 statements/regions và 32 fields;
headroom cuối pipeline; hash allocation phải đủ cho mọi key tường minh, kể cả
key số trỏ vào array. Unknown key trong một prefix đã gộp mà sau nó còn effect
chưa chứng minh được vẫn giữ refusal. Model/role của producer ledger không đổi.

Một unit test mới đối chiếu prefix rỗng/có field, key total/có thể lỗi, store
lặp, thứ tự callback và key validation trước callback tiếp theo. Negative
controls giữ escape, self-reference, capacity và trường hợp hash=0/array>0.
Fixture `table_open_list_split_prefix` kiểm tra repeated fields, nil stores,
varargs, length/next, thứ tự callback và key lỗi ở O0/O1/O2.

## Kết quả

Binary nghiệm thu SHA256: `8c5d3feb9facb0a3dc050e855da3fd68cf50401faf0752be96cccf24b548fb96`.

- Workspace: 1.697 passed, 0 failed, 2 ignored. So với checkpoint trước,
  thêm một unit test mới và một test của example benchmark đã có nhưng chưa
  nằm trong lần chạy trước.
- Fixture and Luau audit: đủ 33 bước đạt; semantic 801/801, deep review 469/469.
- 31 seeds từng bị từ chối cộng seed 8472, mỗi seed 9 profiles: 288/288 đạt.
- 54 table profiles, 108 so sánh VM và 1.476 table probes đều đạt, không fallback.
- 798 semantic, 513 public và 7 residual outputs cũ byte-identical. Giữ nguyên
  885/72/38 rebuilt-call annotations tương ứng.

Fixture mới thêm 95/95/111 dòng ở O0/O1/O2, gồm 9 rebuilt calls ở O2. Oracle
thêm 6 protos: 3 exact và 3 bounded nonequiv; tất cả hàng baseline cũ không đổi,
không có capture excess. Các khác biệt mới là LOADNIL, store trùng được compiler
bỏ và bảng/store kiểm tra key được giữ để lỗi xảy ra trước callback. VM controls
qua tất cả profiles; nonequiv không phải chứng minh runtime sai hoặc tương
đương đầy đủ. Baseline chỉ cập nhật sau khi đọc delta này.

Evidence tại `/workspace/work/v27`: `audit-24`,
`open-tail-prefix-guard-workspace.log`, `open-tail-prefix-final-refusals`,
`table-split-prefix-final-probes`, `table-split-prefix-final-*` và các
`*-comparison.json`. JSON metadata/cache lớn được gzip lossless, kiểm tra
SHA256 trước khi thay bản raw; source outputs và log giữ nguyên.

Broad run 135.000 profiles và 9.000 mutation profiles trước sửa là evidence của
`02173c3`, không phải binary này. Đã chạy lại toàn bộ seeds từng bị từ chối/lỗi;
broad run trên binary cuối của goal vẫn tiếp tục. Benchmark riêng tại
`02173c3` là checkpoint lịch sử, G1 chưa đạt. Không có private dumps hoặc
Windows timing; chưa đóng các review cases thiếu reproducer/đặc tả.
