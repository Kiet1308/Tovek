# Đồng thuận naming và literal object names

Ngày: 27/10/2026, UTC+7. Baseline: `c3a7e41`. Nhánh: `v2.7-dev`.

## Thay đổi

`local chosen; if flag then chosen = model.Head else chosen = model.Tail end` từng nhận tên `head` nếu có nhiều lần đọc. Hai hint có cùng priority nên nhánh gặp trước thắng, trước khi final phi refinement có thể xử lý. Usage census hiện hữu nay thu thập đồng thuận RHS trước khi Namer phát hint cho binding hoặc dùng hint đó để đặt tên binding khác. Unknown/multret writes chặn hint hẹp; parameter còn có giá trị đầu vào nên không lấy tên chỉ từ lần gán sau. Các rule constructor/getter/predicate/qualified lookup cùng giữ veto đó. Không thêm lượt duyệt AST hoặc giữ thêm RcLocal owners.

Nil giữ nghĩa vai trò tùy chọn; copy chính local và phép tự cập nhật số học giữ noun hiện hữu. Đây là naming heuristic, không phải type/effect proof. `dot = -dot` vẫn giữ `dot`; ghi một nguồn khác như `model.Height` vẫn phá đồng thuận. Hai nhánh cùng lấy `.Head` giữ `head`; Head/Tail nhận tên trung tính `selected` sau final refinement. Parameter nhận giá trị chưa rõ rồi một nhánh gán `.Head` không còn bị gọi là `head`.

Literal `.Name = "Greeting"` và constructor `{ Name = "Greeting" }` nay cung cấp tên `greeting`. Các store phải đồng thuận, không có dynamic Name value, unknown multret slot hoặc dynamic key có thể ghi Name. Evidence này không thắng source binding, không thêm annotation và không chứng minh API hay constructor thuần. Rule giữ các gate tránh chặn temp/diamond cleanup. API naming table, module/self roles, tuple roles, scope suffixes và frame/register headroom vẫn dùng cơ chế hiện hữu.

Hai fixtures `naming_branch_consensus` và `naming_literal_object_name` kiểm tra round-trip cả O0/O1/O2; tests AST kiểm tra thứ tự hai nhánh, disagreement, unknown/multret writes, parameter input, constructor/getter, arithmetic updates, literal Name conflicts và source-name priority. Test template cũ được đổi từ đòi tên `huge` sang kiểm tra field MaxSpeed nằm trong constructor đúng một lần: parameter giờ giữ vai trò `maxSpeed`, thay vì tên global chỉ được gán trên một nhánh.

## Kiểm chứng và output

Candidate cuối đạt 1.687 workspace tests, 2 ignored; 786/786 semantic round-trips; 445/445 deep-review cases và đủ 33 bước thực tế của Fixture and Luau audit (`audit-14`). Sáu release probes O0/O1/O2 đều giữ runtime; assertion riêng kiểm tra Head/Tail không lấy tên nhánh đầu, parameter unknown không thành head, literal Greeting và các tên weight/dot vẫn đúng.

Đọc toàn bộ diff và kiểm tra token trên 780 semantic, 513 public và 7 residual outputs cũ:

| Suite | File đổi | Dòng | Dòng không rỗng | Rebuilt calls |
|---|---:|---:|---:|---:|
| Semantic cũ | 54/780 | 47.656 → 47.656 | 40.728 → 40.728 | 870 → 870 |
| Public | 49/513 | 40.608 → 40.608 | 31.990 → 31.990 | 72 → 72 |
| Residual | 5/7 | 3.826 → 3.826 | 3.070 → 3.070 | 38 → 38 |

Mỗi file giữ nguyên số dòng và call annotations. Các token đổi chỉ là identifier, kể cả local trong string interpolation; literal text, field/method names và thứ tự token giữ nguyên. Ngoại lệ duy nhất là `RemoteProperty:GetFor` O0: tên trở về trung tính cho phép pass terminal-return hiện hữu đổi hai store cuối nhánh thành return. Thứ tự test/field read giữ nguyên; fixture mới kiểm tra nil, giá trị riêng, sentinel và lookup có side effect. Các tên hẹp sai như parameter `huge` trở về consumer role `maxSpeed`; biến có nhiều nguồn không đồng thuận dùng tên trung tính.

Oracle: 262 input / 1.629 prototypes, 1.172 exact, 140 equiv, 261 differ, 40 missing và 16 extra; tổng nonequiv 317. 260 input cũ giữ 311, không có per-file regression hoặc capture excess. Hai fixture mới thêm 5 và 1 nonequiv: nhánh/bare nil-init bị compiler bỏ, terminal return đổi chỗ và constant-slot allocation; runtime gates đều đạt. Không coi bounded oracle là chứng minh tương đương đầy đủ. Baseline oracle và output-size được cập nhật sau khi đọc delta.

N1–N3 còn dùng shared tiers, consumer evidence, versioned API roles, module/self/B3 vetoes, tuple roles, scope suffix và bounded rehoist frame đã có; [audit implementation](audit_existing_m4_m6.md) chỉ rõ cơ chế và tests. Không thêm dependency, lượt AST census riêng hoặc tên từ AI.

## Đo chi phí và giới hạn

Ordinary release A/B với `c3a7e41`, cùng 513 inputs, CPU affinity, một warmup và bảy lượt xen kẽ cho mỗi binary/worker count; mọi lần lặp của cùng binary giữ output byte-identical. Median 1 thread: 0,700046 → 0,689332 s (-1,5%), RSS 32.864 → 32.836 KiB. Median 4 threads: 0,198148 → 0,196108 s (-1,0%), RSS 55.464 → 59.572 KiB. Đây là số đo chi phí local của feature, không phải kết luận đã đạt mục tiêu hiệu năng so V2.5.1/V2.6.1.

Evidence được giữ tại `/workspace/work/v27`: `naming-consensus-{semantic,public,residual}-comparison.json`, các patch, identifier review, `naming-consensus-measure/report.json`, oracle, release probes và `audit-14`. Lineage JSON dung lượng lớn được nén lossless bằng Python, kiểm SHA256 trước khi bỏ bản thô; giữ manifest và toàn bộ report/output.

Không có bốn frozen dumps/private review seeds hoặc timing trên máy Windows gốc. Chính sách empty-hash compatibility giữ nguyên giới hạn đã chốt. L3, hiệu năng và nghiệm thu cuối của goal chưa đóng.
