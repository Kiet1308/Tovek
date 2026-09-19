# Roadmap sửa chất lượng output V2

Ngày chốt bằng chứng nghiên cứu: **12/09/2026**. Trạng thái triển khai: **F1, F3, F4, F5, F6 và F8 nền đã nghiệm thu; F2 hoàn thành một phần; F7 và F8 bàn giao còn mở**. Checkbox và commit cập nhật ở mục 4. Đây là roadmap bổ sung cho [ROADMAP_V2.md](ROADMAP_V2.md), không thay trạng thái R1–R9 đã ghi trong [implementation record](roadmap_v2_implementation.md).

**Kết luận:** V2 hiện cải thiện rõ tên biến, cây UI và nhiều ca bảo toàn hành vi, nhưng còn làm công thức toán và một số helper ngắn khó đọc hơn beta. Ví dụ `local styledTextLabel = require(...); v.StyledTextLabel = styledTextLabel` là vấn đề chung của cả hai bản, không phải lỗi riêng V2. Vấn đề này đã được sửa trong source V2 và xuất lại toàn bộ output. Những điểm lùi còn lại cần sửa theo từng loại biểu thức và bằng chứng thứ tự đánh giá; không thể an toàn bằng cách xóa mọi biến dùng một lần.

## 1. Phiên bản, phạm vi và cách đọc số liệu

| Nhãn trong tài liệu | Bản được đo | SHA-256 executable |
|---|---|---|
| Beta / V1 trong cặp so sánh | Release `v0.9.0-beta`, commit `27422b9f8e0aeca0e61db55992930f17a98f4099` | `eb39366eb2428bbdda725376d6b205604d06296e4cc21b0d97eb1612676a6a87` |
| V2 trước F1 | Base `fffb4ad7e738e45288a838cdfd7a111920aaa8c1`, gồm đợt output polish trước | `4641474882013bcc096c3ba8625abe8271e40ec12a4ceccf2bdc045d5d624872` |
| **V2 baseline nghiên cứu (F1)** | Sau output polish + sửa import-field-store F1; đã commit tại `c08a908` | `f582dc87610d866bfb5de78a3f58a216f015839dd4b5b0837386f6af485a9d28` |

Hash từng file source thay đổi, fixture, compiler và báo cáo nằm trong [nghiệm thu F1](roadmap_v2_acceptance/import_field_store_validation.json). Các số đo và cách gọi “V2 hiện tại” trong phần phân tích 2–3, 5 bên dưới chỉ bản F1 đã ghim khi nghiên cứu; kết quả của các bước sửa tiếp theo nằm tại [tiến độ triển khai](roadmap_v2_fix_implementation.md). Beta lấy từ [release chính thức](https://github.com/Kiet1308/Tovek/releases/tag/v0.9.0-beta); folder beta được giữ nguyên hash để còn đối chiếu.

Phép nghiên cứu gồm:

- **3.978 đường dẫn private**: 3.936 input không rỗng và 42 input rỗng; có 3.350 nội dung bytecode không rỗng khác nhau. So sánh toàn bộ bằng hash, diff, AST và compiler; đọc tay các file tiêu biểu và các điểm bất lợi nổi bật. Không coi các bản sao là bằng chứng độc lập và không khẳng định đã đọc tay từng dòng của toàn bộ game.
- **513 cấu hình public** từ 171 source đã pin, O0/O1/O2 g1. Có 405 cấu hình đo được AST ở cả hai bản, 108 vượt budget alignment. Ba cấu hình của cùng source không phải ba chương trình độc lập.
- **198 cấu hình runtime có sẵn**, thêm **6 cấu hình của fixture import mới**. Fixture mới chạy 168 tổ hợp tình huống mỗi cấu hình; kiểm trace, kết quả, lỗi, capture và số lượng return.
- Luau compiler/VM/AST được pin tại commit `c2ec0d4e5ca50796ba174a7565298f59aa572268`, compiler dùng `--fflags=false`. Các module private không được chạy thành game hoàn chỉnh trong Roblox.

Số liệu tổng hợp và các file làm bằng chứng nằm trong [output_weakness_audit.json](roadmap_v2_acceptance/output_weakness_audit.json). Dữ liệu đầy đủ ở `out/v2-fix-research/`; folder đối chiếu đã đồng bộ tại `D:/Medal/V2-vs-beta-v0.9-20260912/`, gồm `index.html`, `BAO_CAO.md`, hai folder output, diff và 25 trang đọc song song.

### Toàn cảnh sau F1

| Chỉ số | Beta | V2 trước F1 | V2 hiện tại |
|---|---:|---:|---:|
| File private parse/compile O0 và O2 được | 3.975/3.978 | 3.978/3.978 | 3.978/3.978 |
| Dòng private | 493.350 | 518.284 | **508.007** |
| Byte private, LF | 12.951.033 | 13.926.466 | **13.624.954** |
| Cặp `local x = require(...); table.Field = x` liền nhau | 10.045 | 10.007 | **0** |
| File chứa đúng cặp `styledTextLabel` người dùng nêu | 544 | 544 | **0** |
| AST ratio public trung bình, 405 cấu hình chung | 0,8265 | 0,8526 | **0,8637** |

V2 hiện vẫn dài hơn beta **2,97% dòng, 5,20% byte**. Có 3.048 file private đổi, 930 file giống hệt. Trên 3.975 file cùng parse được, binding dạng `p/v + số` giảm 54.058 → 36.955, tức 31,64%; đây không phải phần trăm khôi phục đúng tên tác giả. Trên 405 cấu hình public chung, ratio hiện tăng ở **215**, giảm ở **61**, bằng ở **129** so với beta. Điểm trung bình tốt hơn không phủ nhận 61 ca giảm và không chứng minh tương đương runtime. [Bằng chứng tổng hợp](roadmap_v2_acceptance/output_weakness_audit.json).

## 2. Phân tích điểm chưa tốt và nguyên nhân

### 2.1. Import chỉ chuyển tiếp vào field — vấn đề chung, đã sửa F1

Trong cùng file `ReplicatedStorage/FusionPackage/Components/Tooltips/Styling/Label.luau`, beta và V2 trước F1 đều có:

```luau
local styledTextLabel = require(fusionPackage.Components.Base.StyledTextLabel)
v.StyledTextLabel = styledTextLabel
```

V2 hiện xuất:

```luau
v.StyledTextLabel = require(fusionPackage.Components.Base.StyledTextLabel)
```

Tên field đã nói rõ vai trò. Alias chỉ xuất hiện một lần ở phép gán này nên việc giữ thêm tên import không giúp người đọc. Tuy nhiên, import dùng làm hàm gọi là ngữ cảnh khác: `local makeThunkMiddleware = require(...); return makeThunkMiddleware(...)` vẫn có ích và vẫn được giữ.

**Nguyên nhân đã xác nhận trong code:** bộ inline bảo vệ `require`/`GetService` khá rộng, cả ở [SSA inline](../cfg/src/ssa/inline.rs#L337) và cleanup AST. Quy tắc này giúp tránh mất tên import hữu ích, nhưng cũng chặn alias chỉ chuyển tiếp sang một field có tên. F1 thêm trường hợp xét duyệt theo đúng vị trí dùng tại [inline_temps.rs](../ast/src/inline_temps.rs#L226), với bộ nhận diện [named field store](../ast/src/inline_temps.rs#L337).

F1 chỉ cho phép ứng viên đi qua vòng xét duyệt; vẫn cần một lần đọc, một lần ghi, không capture alias, giữ binding nguồn, không vượt statement có hiệu ứng và không đảo vị trí đánh giá. Receiver có lookup lồng, key động, receiver bị closure thay đổi, alias dùng hai lần hoặc có binding nguồn cần giữ đều được kiểm soát. Tên `require` không được dùng để kết luận lời gọi là pure.

**Kết quả:** 1.300 file đổi so với V2 trước F1; giảm 10.277 dòng và 301.512 byte. Toàn bộ 10.007 cặp liền nhau được scanner nhận diện đã biến mất. Mức giảm dòng lớn hơn số cặp do các cleanup tiếp theo và trường hợp import/service khác; không quy toàn bộ 10.277 dòng thành 10.277 require. Binding `p/v` lại tăng 2 trên toàn corpus, cho thấy chỉ số này bỏ sót lợi ích của việc xóa alias vốn đã có tên đẹp. [Nghiệm thu F1](roadmap_v2_acceptance/import_field_store_validation.json).

F1 còn cải thiện 15 cấu hình thuộc 5 source public: Fusion `src/init.luau`, roact `src/init.lua`, RbxUtil `comm`, `input`, `streamable`. Đặc biệt Fusion init từ 0,3293 trước F1 lên 0,9439, cao hơn beta 0,6553: require đã vào constructor export. **Không tiếp tục liệt kê Fusion init là điểm lùi hiện tại.** [Source Fusion đã pin](https://github.com/dphfox/Fusion/blob/2790f7b6272bdf7cd0bbfee259a2f9d79ea20810/src/init.luau), [số liệu các phiên bản](roadmap_v2_acceptance/output_weakness_audit.json).

### 2.2. Công thức toán và đối số bị chia thành nhiều local — điểm lùi lớn nhất còn lại

Đếm trên cùng 3.975 file parse được; các số baseline dưới đây gồm cả reference qua closure. Scanner F8 tách thêm nhóm captured/uncaptured để không xem mọi local dùng một lần là ứng viên được phép di chuyển:

| Dấu hiệu AST | Beta | V2 hiện tại | Số file tăng |
|---|---:|---:|---:|
| Local tên sinh tự động, dùng một lần, initializer là Binary | 1.374 | **4.843** | 874 |
| Tương tự, initializer là Unary | 43 | **204** | 79 |
| Local dùng một lần, initializer đọc field có tên | 3.579 | **6.873** | 773 |
| Local tên sinh tự động, dùng một lần, initializer là Call | 4.659 | **2.811** | 333 |

“Dùng một lần” ở đây là đếm reference AST theo declaration, không phải bằng chứng được phép inline. Số Call tạm tổng thể đã giảm; vấn đề nổi bật nằm ở toán, snapshot field và một số vị trí đối số, không phải mọi lời gọi đều tệ hơn. [Chi tiết scanner và top file](roadmap_v2_acceptance/output_weakness_audit.json).

Những file nên dùng làm mục tiêu đánh giá:

| File private | Dòng beta → V2 hiện tại | Biểu hiện |
|---|---:|---|
| `Shared/Zone/Geometry/init.luau` | **384 → 658** | Công thức hình học bị tách nhiều field snapshot và bước toán; statement 235 → 482, byte tăng 57,8% |
| `DivergentVFX/LightningCore.luau` | **3.258 → 3.682** | `randomUnit`/`randomHemisphere` có nhiều `v`, `v2`, `v3` nối `math.max`, `math.sqrt`, `Vector3.new`; Binary tạm dùng một lần 32 → 126 |
| `Shared/Network/BufferEncoder/Write.luau` | **1.942 → 1.458** | Tổng thể tốt hơn, nhưng Binary tạm dùng một lần 1 → 73; không đánh đồng ít dòng với biểu thức đã tốt |
| `MoonPlayer/LerpCore/BoatTween/TweenFunctions.luau` | Xem audit | Binary tạm dùng một lần 0 → 50; nhóm toán nên thêm vào tập đánh giá |

**Vì sao V2 kém gọn hơn:** các lần đọc global/field, phép toán có thể gọi metamethod và lời gọi hàm được giữ đúng thứ tự chặt hơn. Trong [evaluation_order::can_sink](../ast/src/evaluation_order.rs#L156), hai sự kiện không chứng minh được total-pure có thể tạo xung đột. Ví dụ đưa `local X = data.X` vào `math.isfinite(X)` có thể chuyển việc tra `math.isfinite` lên trước lần đọc `data.X`. Nhìn bằng mắt giống công thức tương đương, nhưng môi trường có thể làm hai thứ tự khác nhau.

Luau có thể resolve chuỗi global bằng import khi load và chỉ dùng đường nhanh trong môi trường phù hợp; `getfenv`/`setfenv` có thể ảnh hưởng giả định đó. Vì vậy không gắn nhãn pure chỉ vì tên bắt đầu bằng `math` hay vì output có annotation `number`. [Tài liệu Luau về import/global và môi trường](https://luau.org/performance/). Bản VM đã pin kiểm `safeenv` tại `VM/src/lvmload.cpp:200` và `VM/src/lvmexecute.cpp:501`; đây là điểm cần nối với provenance của bytecode, không phải lý do mặc định mọi global lookup đều bỏ được.

**Hướng sửa F2:** trước hết tìm các vị trí vẫn giữ nguyên chuỗi sự kiện mà cleanup bỏ lỡ. Với trường hợp cần đi qua lookup, chỉ cho phép khi có chứng cứ import origin, binding ổn định và hợp đồng môi trường đủ mạnh. Khi không có chứng cứ, giữ snapshot nhưng dùng tên hoặc bố cục tốt hơn. Không thay toàn bộ effect gate bằng danh sách tên API “quen thuộc”.

### 2.3. Helper điều kiện ngắn dài ra — một phần do chính sách, một phần cần sửa

`FusionPackage/Components/Base/Billboards.luau` tăng 842 → 1.027 dòng. Helper `isFinite` hiện đọc X/Y/Z thành local, đưa kết quả vào `v5`, rồi cập nhật qua nhiều nhánh. Chữ ký và tên helper tốt hơn, nhưng người đọc khó nhìn ra ba phép kiểm tra tạo thành một điều kiện ngắn. Fusion `castToGraph` public cũng giữ được tên hàm nhưng từ điều kiện `and` của source thành local và các guard; O2 ratio 0,8815 → 0,5358. [Source castToGraph đã pin](https://github.com/dphfox/Fusion/blob/2790f7b6272bdf7cd0bbfee259a2f9d79ea20810/src/Graph/castToGraph.luau), [số đo](roadmap_v2_acceptance/output_weakness_audit.json).

V2 chủ động ưu tiên statement: [pipeline](../luau-lifter/src/lib.rs#L744) gọi bộ dựng short-circuit, không bật lại mọi if-expression. [Conditional reconstruction](../ast/src/conditional_expressions.rs#L48) và [lowering](../ast/src/lower_conditionals.rs#L194) còn có budget và ràng buộc. Vì vậy AST ratio giảm ở source vốn dùng if-expression không tự động là lỗi; nhưng `local flag; if ... then flag = ... end; return flag` vẫn có thể có cách trình bày tốt hơn trong chính quy ước statement.

**Hướng sửa F3:** dựng guard/direct return và chuỗi short-circuit khi giữ đúng giá trị lẫn thứ tự, giảm binding trung gian khi chỉ nối các nhánh. Không đổi mặc định về if-expression. Không chuyển tùy tiện `if c then a else b` thành `c and a or b`: khi `a` là false/nil, kết quả có thể khác; if-expression chỉ chạy một nhánh. [RFC Luau về if-expression](https://rfcs.luau.org/syntax-if-expression.html). Các ca NaN, false/nil, lỗi ở từng nhánh và capture mutation phải nằm trong nghiệm thu.

### 2.4. Bảng component vẫn là `local v = {}; v.Field = ...` — phần tiếp theo sau F1

File Label hiện vẫn có:

```luau
local v = {}
v.Block = require(fusionPackage.Components.Base.Block)
v.StyledBlock = require(fusionPackage.Components.Base.StyledBlock)
v.StyledTextLabel = require(fusionPackage.Components.Base.StyledTextLabel)
-- Các field khác; v được dùng trong closure phía dưới.
```

Đây là trích đoạn, không phải toàn bộ output. Dạng mục tiêu, **nếu chứng minh bảng chưa bị quan sát trước khi khởi tạo xong**, là constructor có field theo thứ tự cũ và tên bảng có vai trò, chẳng hạn `components`. Tên đó là suy luận, không phải tên nguồn đã tìm lại.

**Nguyên nhân đã thấy:** [rebuild_table_literals](../ast/src/rebuild_table_literals.rs#L96) từ chối bảng nằm trong tập captured; [constructor-region sink](../ast/src/rebuild_table_literals.rs#L263) cũng có gate này. Với file Label, bảng được closure bên dưới giữ lại nên thuộc diện bảo vệ. Quy tắc theo toàn hàm chưa phân biệt “chỉ capture sau khi init xong” với “closure có thể đọc bảng đang init”.

**Hướng sửa F4:** phân tích escape/capture tại từng điểm chương trình, không bỏ `captured.contains` hàng loạt. Cần giữ thứ tự cấp phát, require, store, lỗi, nil field, key trùng/key động và quan sát bảng chưa hoàn chỉnh. Bảng được đưa cho hàm ngoài hoặc closure được gọi giữa hai phép gán phải bị từ chối khi chưa có proof. Không xóa các require trùng/không lấy return chỉ vì trông dư: lời gọi vẫn có thể có hiệu ứng.

### 2.5. Tên có nghĩa nhưng sai sắc thái hoặc ngữ pháp

RbxUtil `Ser.SerializeArgs` và `DeserializeArgs` đặt tên bảng đóng gói là **`serializeds` / `deserializeds`**. Đây là tên đọc kém tự nhiên hơn `result` của beta và không diễn đạt rõ bảng chứa cả các giá trị chưa cần chuyển đổi. Trong `SerializeArgsAndUnpack` còn thêm snapshot `n` trước `table.unpack`. [Source Ser đã pin](https://github.com/Sleitnick/RbxUtil/blob/31f9120fca021e3dec275b42bc7047d626962082/modules/ser/init.luau), [output hiện tại và metric](roadmap_v2_acceptance/output_weakness_audit.json).

Rodux `prettyPrint` nhận được scalar/string/table nhưng V2 gọi tham số là **`items`** do một nhánh lặp table; tham số mức thụt lề thành **`value: number?`**. Đó là ví dụ tên dựa trên một cách dùng cục bộ chưa phản ánh toàn bộ hàm. O2 ratio 0,9291 → 0,4248 còn chịu ảnh hưởng của nhiều alias và bố cục, không thể quy toàn bộ giảm điểm cho rename. [Source prettyPrint đã pin](https://github.com/Roblox/rodux/blob/120e94ec609c4992c9f2da7c7ae45542b168fd25/src/prettyPrint.lua).

**Cơ chế liên quan đã xác nhận:** [pluralize](../ast/src/name_locals.rs#L1388) xử lý hình thái từ chủ yếu bằng hậu tố; [collection-content consensus](../ast/src/name_locals.rs#L3070) có thể đưa tên suy ra từ phần tử lên mức ưu tiên 52/53. Việc lan truyền một gợi ý yếu thành tên tập hợp cần giữ độ tin cậy và loại từ. [Refine-names propagation](../ast/src/refine_names.rs#L637) là điểm cần xem cùng cơ chế chọn ứng viên; không chỉ vá riêng hai từ `serializeds`.

**Hướng sửa F5:** chỉ pluralize danh từ hoặc vai trò đã có đủ bằng chứng; cân nhắc `args`, `values`, `result` khi dữ liệu hỗn hợp. Gợi ý từ loop không được tự lấn át bằng chứng scalar/string ở nhánh khác. Giữ nguyên tên debug/source đã ghi nhận và tên getter có nghĩa mạnh. `width/height`, `processor`, `size`, `part/parts` đang tốt phải nằm trong tập bảo vệ. Các tên đích như `indentLevel` chỉ được dùng khi có ngữ cảnh đủ mạnh, không hard-code theo tên file thư viện.

### 2.6. Vị trí helper, scope và cleanup cuối pipeline chưa đều

roact `createSignal` có lại tên `createSignal`, `fire`, `disconnect`, nhưng `fire` được đưa lên trước callback subscribe còn anonymous; assertion có thêm local. O2 ratio 0,7125 → 0,4582; O0 lại cải thiện. Đây là thay đổi pha trộn: tên hàm tốt hơn trong khi bố cục xa source. [Source createSignal đã pin](https://github.com/Roblox/roact/blob/1676d95c4886d51ee2b21bfcd55c6e50ece799e5/src/createSignal.lua), [metric theo cấu hình](roadmap_v2_acceptance/output_weakness_audit.json).

**Giả thuyết cần kiểm tra, chưa khẳng định là nguyên nhân duy nhất:** thứ tự pass có thể để lại alias mới sau cleanup sớm. Pipeline hiện chạy naming/method/inline, short-circuit, cleanup UI, receiver materialization, de-inline, rebalance và normalize condition. [Pipeline hiện tại](../luau-lifter/src/lib.rs#L714). Cần ghi lại thời điểm mỗi binding xuất hiện và lý do từ chối trước khi reorder hoặc thêm vòng fixed point.

**Hướng sửa F6:** policy đặt helper tại scope nhỏ nhất vẫn giữ tần suất tạo closure, identity, recursion, capture epoch và dependency; kèm một lượt cleanup giới hạn nếu có bằng chứng alias mới xuất hiện sau pass. Không di chuyển function declaration chỉ để tăng AST ratio, vì closure tạo mỗi lần gọi khác closure được chia sẻ ngoài hàm.

### 2.7. Annotation và discard tạo nhiễu, nhưng không phải code chết mặc định

Trên cùng 3.975 file parse được, marker suy luận call tăng 0 → **529** ở 198 file; toàn V2 có **531**. `local _ = ...` tăng 3.107 → **3.183** trên tập chung, toàn V2 có 3.185. Những marker lặp nhiều làm code đọc nặng hơn; các discard field lookup như `local _ = fusion.OnEvent` có vẻ thừa, nhưng lookup vẫn có thể gây lỗi hoặc gọi metamethod. [Audit](roadmap_v2_acceptance/output_weakness_audit.json).

**F7 chỉ cần hoàn thiện cách hiển thị:** đã có `--compact-annotations`; [formatter](../ast/src/formatter.rs#L1819) chỉ thu gọn khi emission map còn lưu được nội dung đầy đủ, nếu không phải giữ comment. Không lên kế hoạch “thêm compact mode” như tính năng chưa có, không xóa dấu suy luận khiến người đọc tưởng call site đã được khôi phục chắc chắn. Với discard, chỉ bỏ khi chứng minh biểu thức total-pure; nếu không, giữ một cách biểu diễn rõ ràng.

## 3. Những điểm tốt phải giữ và những điều chưa chứng minh

V2 hiện có ba sửa lỗi cú pháp private đã kiểm parser/compiler. Các file UI vẫn cho thấy tiến bộ cụ thể: `StageInfo` binding p/v 80 → 10, statement 187 → 143; `StatsWindow` 102 → 18 và 191 → 156; `GameUnitView` 84 → 11 và 295 → 181. `mesh` giảm 1.360 → 832 dòng; `Write` giảm 1.942 → 1.458 dòng dù còn vấn đề biểu thức; sRGB dựng lại helper màu. Các số này là các khía cạnh của chất lượng, không thay thế kiểm hành vi. [Các hàng so sánh đã ghim](roadmap_v2_acceptance/output_weakness_audit.json).

`makeThunkMiddleware` bị mất tên import và `partOnRayWithIgnoreLists` quá dài đã được sửa ở đợt output polish trước; hiện không còn là regression mở. Fusion init đã được sửa thêm trong F1. Tránh dùng screenshot/output cũ làm bằng chứng cho bản `f582dc87610d…`.

Chưa phát hiện lỗi runtime mới trong các phép thử đã chạy cho F1. Điều này không chứng minh toàn bộ private game tương đương: public AST chỉ so cấu trúc; symbolic dataflow của cả 6 cấu hình import mới là `unknown`; các tình huống VM là kiểm thử hữu hạn. Một snapshot làm code dài có thể đang bảo vệ lỗi/capture/thứ tự lookup thật, nên phải xem trace trước khi gọi nó là “rác”.

## 4. Thứ tự triển khai và tiêu chí nghiệm thu

| Mục | Ưu tiên / trạng thái | Kết quả cần đạt | Điều kiện và độ khó |
|---|---|---|---|
| **F1 — import → field** | P0, **xong** | Gộp relay một lần tại vị trí an toàn, giữ import callee hữu ích | Đã chạy source/runtime/metadata/corpus; không cần AI |
| **F2 — biểu thức toán và đối số** | P1, **xong bước proof số**, còn mở | Công thức dễ nhận ra hơn, giảm temp thừa trong nhóm Geometry/Lightning/Write/timer/prettyPrint | Commit `2add294`; 13 file cải thiện, các nhóm snapshot còn lại đang xử lý |
| **F3 — helper điều kiện ngắn** | P1, **xong** | Ít binding phi/flag hơn, guard và return gọn trong chế độ statement | 754 test AST, 222+6 runtime, 513 public và metadata qua; 79 file private cải thiện, không tăng dòng dài |
| **F4 — constructor trước capture** | P1, **xong** | Gom bảng component/export chỉ capture sau init khi chưa escape | Commit `e1f1131` đã push; 1.215 file private được gom field, runtime/metadata qua |
| **F5 — chất lượng tên suy luận** | P1, **xong** | Hết plural sai và tên bị một nhánh sử dụng chi phối | Commit `b66a186` đã push; 744 test AST; 222 runtime, 513 public, metadata qua; 86 file private cải thiện tên, khôi phục `size` ở Write |
| **F6 — scope/helper và pass cuối** | P2, **xong** | Helper dùng một lần vào constructor đã có callback inline; giữ danh sách helper riêng | Trace 11 stage; 4 public output đổi, private giữ nguyên; [nghiệm thu](roadmap_v2_acceptance/fix_helper_placement.json) |
| **F7 — annotation/discard** | P2, mở | Hiển thị nhẹ hơn nhưng vẫn thấy đâu là suy luận | Vừa; dùng compact mode sẵn có và metadata đầy đủ |
| **F8 — gate chất lượng output** | **Nền đã xong**, gate bàn giao còn mở | Phát hiện lùi theo file/nhóm, giữ unknown và baseline bất biến | Commit `f1059d9` đã push; scanner tích hợp report, fixture mặc định hiện có 228 profiles |

Trình tự đề xuất: **F1 đã hoàn thành → phần gate tối thiểu F8 → F2 → F3 → F4 → F5 → F6 → F7**, chạy F8 sau mỗi thay đổi. Ưu tiên output theo yêu cầu; R7 performance để sau. **R9 tiếp tục dừng; AI tắt mặc định; không tải model, không đưa output/private source lên GitHub.**

Checklist triển khai:

- [x] **F1:** sửa source theo ngữ cảnh field store; thêm fixture owned và unit tests phủ cả ca được gộp lẫn ca phải giữ; xuất lại corpus, folder V2 và HTML.
- [x] **F8 nền:** tích hợp scanner các loại temp/relay/annotation vào report thường xuyên; thêm fixture import vào lệnh nghiệm thu chuẩn; giữ baseline hash và tách số file, source duy nhất, cấu hình và unknown. Đã push `f1059d9`; [bằng chứng](roadmap_v2_acceptance/fix_quality_foundation.json).
- [ ] **F2:** thu trace lý do từ chối ở các điểm toán tiêu biểu; triển khai từng proof nhỏ và chứng minh giảm local không đánh đổi event order. Nghiệm thu phải có ít nhất một ca cải thiện trong mỗi nhóm đã nhận vào scope; ca chưa đủ proof ghi rõ còn mở, không che bằng tổng trung bình.
- [x] **F3:** khôi phục boolean chain/guard/direct return khi value-exact; kiểm cả false/nil, NaN, lỗi giữa nhánh, branch bị bỏ qua và mutation; không thêm if-expression vào default output. Giữ arity, source binding, parameter và upvalue; từ chối chuỗi dài hoặc cách viết che khác biệt false/nil. [Bằng chứng](roadmap_v2_acceptance/fix_terminal_returns.json).
- [x] **F4:** chứng minh bảng chưa escape trước init hoàn tất; gom constructor theo thứ tự và giữ các ca capture sớm/callback/reentrant/alias quan sát bảng dở dang. File Label và nhóm component registry đã đọc lại. Đã push `e1f1131`; [bằng chứng](roadmap_v2_acceptance/fix_constructor_capture.json).
- [x] **F5:** lưu độ tin cậy và loại vai trò xuyên các lần suy luận; sửa tên collection/polytype tổng quát; không còn `serializeds`/`deserializeds` do heuristic trong source benchmark hiện tại; bảo vệ tên nguồn và các ví dụ người dùng đã duyệt. [Bằng chứng](roadmap_v2_acceptance/fix_naming_roles.json).
- [x] **F6:** trace 11 stage cho createSignal/prettyPrint/castToGraph; sửa đúng gate helper cùng tên và hai capture-read. Kiểm identity, recursion, capture, callback và tần suất tạo closure; không thêm cleanup lặp khi trace không cho thấy alias mới. 234 runtime, 513 public qua; không regression ratio đo được; private giữ nguyên. [Bằng chứng](roadmap_v2_acceptance/fix_helper_placement.json).
- [ ] **F7:** thu gọn annotation với mapping đầy đủ, giữ marker suy luận và fallback khi metadata không đủ; discard chỉ loại bỏ khi có proof effect, kèm parse/span/AST/runtime checks thích hợp.
- [ ] **F8 bàn giao:** chạy gate cuối, đồng bộ folder V2/HTML và 25 trang đối chiếu với executable cuối; kiểm browser, hash và beta bất biến.

**Cập nhật triển khai 12/09/2026:** F2 đã push bước proof số `2add294` nhưng chưa đủ điều kiện tick toàn mục. F3, F4 và F5 đã nghiệm thu. Chi tiết từng bước và giới hạn tại [file tiến độ](roadmap_v2_fix_implementation.md); các số đo F1 trong phần nghiên cứu bên dưới là baseline lịch sử, không phải số đo bản đang phát triển.

Không chốt mục tiêu kiểu “xóa 100% local dùng một lần” hoặc “AST phải đạt 1,0”. Mỗi đợt cần danh sách cụ thể các ca cải thiện, các ca không đổi vì an toàn và mọi ca giảm điểm; chỉ sửa nhận xét đánh giá khi đã xem output mới.

### Ma trận gate cho các đợt tiếp theo

| Gate | Điều kiện qua |
|---|---|
| Parser/compiler | 3.978/3.978 private và 513/513 public vẫn parse/recompile được với toolchain đã pin; output không đổi có thể tái dùng bằng hash |
| Hành vi | 228 cấu hình chuẩn hiện có qua; mỗi proof mới có ca đối chứng mutation/metamethod/error/multret/capture phù hợp |
| F1 không tái phát | Corpus hiện tại vẫn 0 cặp relay đã nhận diện; fixture giữ được source binding, multi-use, nested receiver và mutable captured receiver |
| Source/debug identity | Binding debug/source, arity và capture epoch không bị đổi chỉ để giảm số local; tên ghi nhận mạnh hơn heuristic |
| Metadata | Source analysis/trace giống nhau; thread 1/4 deterministic; không thêm identifier occurrence không giải thích được; lineage của binding đã inline được theo dõi |
| Chất lượng public | So từng profile với **V2 hiện tại** và beta; báo raw/normalized ratio, unknown, tên và nhận xét người đọc riêng biệt; không lấy mean che regression |
| Chất lượng private | Đọc lại Geometry, LightningCore, Billboards, ClickToMove, Utils, Write và các file UI được bảo vệ; thống kê theo nhóm temp, không chỉ dòng/p-v |
| Bàn giao | SHA executable/source/report khớp; folder V2, diff, 25 trang đối chiếu và HTML cùng bản; beta giữ nguyên |

## 5. Nghiệm thu thực tế của F1

| Kiểm tra đã chạy | Kết quả |
|---|---|
| Rust workspace | **1.023 test chính**, thêm 1 lần test con được chạy lại; 0 fail |
| Private | **1.300 file đổi** đều parse và compile O0/O2; 2.678 file còn lại giữ đúng hash bản đã kiểm; tổng 3.978 |
| Runtime cũ | **198/198** cấu hình qua; 9 negative controls của bộ cũ qua |
| Runtime import mới | **6/6** O0/O1/O2 × g1/g2 qua; 168 tổ hợp/profile, tổng 1.008 lượt quan sát/profile-scenario mỗi phiên bản |
| Public | **513/513** parse/recompile qua; so V2 trước F1: **15 tăng / 0 giảm / 390 bằng** trong 405 cấu hình đo được |
| Metadata | Runtime 198, import 6, public 513 qua lineage/capture/emission checks, deterministic thread 1/4; không có local token chưa giải thích được |
| Bàn giao | **9.378 file output** hai bên kiểm hash; browser lọc/sắp xếp/phân trang và 25 gallery khớp output mới; không lỗi JavaScript |

Fixture mới gồm 7 dạng: gán trực tiếp, receiver lồng, call xen giữa, receiver bị closure thay đổi, import dùng hai lần, constructor và import được gọi như hàm. Mỗi dạng chạy nil/false/string/function, nhiều vị trí lỗi, log `require`/lookup/store, mutation và extra return. Source nằm tại [import_field_store.luau](failure_fixtures/roadmap_v2/import_field_store.luau), [driver](failure_fixtures/roadmap_v2/import_field_store.driver.luau), [manifest với expected output](failure_fixtures/roadmap_v2/import_field_store.manifest.json). Tests AST tại [inline_temps.rs](../ast/src/inline_temps.rs#L1134) bổ sung các điều kiện từ chối riêng.

Script [provenance_fixtures.py](../scripts/provenance_fixtures.py#L114) cũng đã sửa một giả định của harness: trước đây mọi fixture report đều phải chứa hai conditional examples O2 g1/g2. Nay script đối chiếu đúng tập conditional profiles có trong input; các kiểm tra nội dung lineage vẫn giữ. Đã chạy lại cả bộ 198 và bộ import 6 sau sửa harness, không bỏ qua lỗi để đánh dấu pass.

Các nguồn bằng chứng để tiếp tục công việc:

- [Nghiệm thu F1: hash, test, runtime, metadata, số liệu trước/sau](roadmap_v2_acceptance/import_field_store_validation.json).
- [Audit điểm yếu: baseline, chỉ số AST, file private và source public tiêu biểu](roadmap_v2_acceptance/output_weakness_audit.json).
- [Đợt output polish trước F1](output_polish_20260912.md), để phân biệt lỗi đã sửa trước với lỗi còn mở.
- `out/v2-fix-research/output-audit-before.json`, `output-audit-after.json`, `private-review.json`, `public-review.json`: dữ liệu chi tiết, scanner và hash được ghim trong hai report trên.
- `D:/Medal/V2-vs-beta-v0.9-20260912/reports/delivery-checks.json` và `browser-checks.json`: kết quả đồng bộ và kiểm giao diện thực tế.

Các kết luận từ scanner là bằng chứng định lượng; nhận xét dễ đọc là đánh giá có ví dụ; nghi vấn về thứ tự pass ở F6 được ghi là giả thuyết. Các mục mở chỉ được đánh dấu hoàn thành sau khi có implementation và kết quả nghiệm thu tương ứng.
