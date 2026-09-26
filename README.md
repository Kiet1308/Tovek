# Tovek

**A high-readability, high-performance Luau decompiler.** **Tovek V2 v0.1**

[**Download V2 v0.1 for Windows or Linux**](https://github.com/Kiet1308/Tovek/releases/tag/v2-v0.1) · [What’s new in V2](https://kiet1308.github.io/Tovek/changelog.html)

Each package includes the CLI, the local HTTP server, client scripts and quick-start instructions. The release tag is `v2-v0.1`; Rust package versions are `0.1.0` within the V2 generation.

[**💬 Join the Tovek Discord →**](https://discord.gg/phY6VUDSF7)

Tovek is a fork of the [medal](https://github.com/Stefanuk12/medal-decompiler) Luau
decompiler, rebuilt around a single goal: **output you can actually read.** Where most
decompilers hand you a wall of `v1, v2, v3 …` and inlined compiler noise, Tovek
reconstructs names, methods, control flow and idioms so the result reads close to the
source a human would have written — without sacrificing correctness.

AI features are disabled. The current decompiler uses deterministic rules and
does not load or download models or call AI services. AI is not part of this release. Model weights and model caches
stay local and must not be committed or uploaded to GitHub, including release
assets, workflow artifacts or Git LFS.

---

## What's new in V2

| | v0.9 beta | **Tovek V2 v0.1** |
|---|---|---|
| Roblox bytecode v12 | Not supported | Native support, including CALLFB |
| Anonymous p/v bindings | 54,058 | 36,826 |
| Lines over 180 characters | 382 | 98 |
| Passing shared runtime profiles | 138 / 198 | 198 / 198 |
| Public parse/recompile profiles | 513 / 513 | 513 / 513 |

Readability counts use the same 3,975 private files parseable in both versions.
The public profiles cover 171 source files at three optimization levels.
See the [release evaluation](https://kiet1308.github.io/Tovek/changelog.html#evaluation)
for scope, methodology and remaining regressions.

### Core capabilities

- **Name inference.** Locals and parameters get meaningful names derived from how they're
  used: `:Connect` → `connection`, `:Clone()` → `clone`, `:LoadAnimation` → `track`,
  `Color3.new` → `color`, `Vector3.new` → `vector`, `GetAttribute("Speed")` → `speed`,
  event signatures → `dt` / `input` / `player` / `child`, `tonumber`/`tostring` results,
  and more — falling back to `v*` only when nothing can be inferred soundly.
- **Service & `require` preservation.** `game:GetService(...)` and `require(...)` handles
  are kept as single named locals at the top of the chunk instead of being folded into
  every use site, so the dependency surface of a script is obvious at a glance.
- **OOP recovery.** Method tables defined with an explicit `self` first parameter are
  rendered back with colon-call syntax (`function T:method()`), the way they were written.
- **Reverses the Luau optimizer.** Tovek undoes `-O2` inlining — single-use temporaries,
  inlined expressions, and exploded table constructors are reassembled. Computed React event
  keys, drained `children` fields, callbacks, props and nested child tables are rebuilt from the
  leaves upward into declarative UI trees, with the original inlining points left as unobtrusive
  trailing comments.
- **Idiomatic cleanup.** Compound assignments, backtick string interpolation,
  left-associated `and`/`or` (far fewer redundant parentheses), atomic `math.pi`, dropped
  needless `\'` escapes, and removal of redundant local copies, constant-only branches and
  discarded pure expressions. Roblox zero/one/identity constructors use their canonical
  properties, callback fields keep assignment syntax, long identifiers stay intact, and
  function-heavy returned tables recover a named module shape. Oversized truthy-selection
  chains return to `if`/`elseif`, while long left-associated concatenations become a named
  accumulator with `..=` updates instead of a parenthesized one-line wall.
- **Modern bytecode coverage.** Reads Luau serialization **v4–v14**, including v12
  size-delimited prototypes, 64-bit cost metadata, call feedback and `CALLFB`, and the
  v14 `FASTPCALL` fast path for `pcall`/`xpcall`, which decompiles to the same source as
  v9 (`pcall(require, script.Parent.Config)`, not a hoisted `local require2 = require`).
  v12 and v14 are each covered by 252 compiler/runtime profiles and the 513 public
  recompile profiles; v12 also has malformed-input checks and executed wasm32 reader
  tests. v13 double-vector serialization has targeted coverage; v15 and the experimental
  class opcodes (`NEWCLASS`) are unsupported. Runtime-mutated `CMPPROTO` guards are explicitly rejected because their
  prototype-identity predicate cannot be faithfully reconstructed in source. See the
  [v12 validation and limits](https://kiet1308.github.io/Tovek/changelog.html#bytecode-v12).
- **Validated output.** Public regression fixtures are checked with Luau's own
  parser, compiler and VM. V2 passes all 513 public parse/recompile profiles and
  the expanded suite of 246 runtime profiles. These checks cover the tested
  inputs; they do not prove equivalence for every program.

### Performance

- Cached analysis facts and binding summaries reduce repeated work in large
  functions, while bounded rescans limit control-flow traversal costs.
- **mimalloc** supplies per-thread allocation caches.
- **Parallel** per-function lifting and folder decompilation use rayon.
- **Deterministic, byte-identical output** across thread counts keeps results
  reproducible and easy to compare.
- An optional bounded artifact cache speeds up repeated folder workflows.

### Better tooling

- A native **`decompile-folder`** subcommand that decompiles an entire SynSaveInstance dump
  in parallel.
- A native **`validate-folder`** subcommand that decompiles *and* validates every output
  against Luau's parser in one pass.
- A small **HTTP server** (`web-server`) for the executor → server workflow, plus a
  ready-to-use client script.
- A **Cloudflare Worker** target (`luau-worker`) for serverless deployment.

---

## Output and validation

The [V2 release article](https://kiet1308.github.io/Tovek/changelog.html#evaluation)
compares the validated V2 output with v0.9 beta, including readability gains,
runtime checks and remaining regressions. The [UI reconstruction example](https://kiet1308.github.io/Tovek/changelog.html#ui-trees)
illustrates how module exports can be reconstructed into a table.

Public regression fixtures, pinned corpus manifests and the CI workflow remain in
this repository. Private bytecode, generated output and internal research reports
stay local. No fresh matched comparison with hosted decompilers is claimed for V2.

---

## Usage

### CLI

Single file (raw Luau bytecode):

```sh
luau-lifter <file.luac>
```

Roblox client bytecode is encoded (`op = op * 203 % 256`); pass `-e` for it:

```sh
luau-lifter <script.lua> -e --script-name "Workspace.Script"
```

Decompile a whole folder of saved-bytecode files in parallel (mirrors the input tree,
renaming `.lua` → `.luau`):

```sh
luau-lifter decompile-folder ./dump ./out          # -e/--key 203 is the default for this mode
```

For repeated folder runs, add `--cache-dir ./tovek-cache`. The optional cache
keys artifacts by the exact binary, bytecode, options and module naming context;
it rebuilds path-specific metadata on each run. Keep the cache outside the input
and output trees. `--cache-max-mib` defaults to 512.

Add `--emit-binding-provenance --compact-annotations` to use short reconstruction
comments while retaining their complete diagnostics and reconstructed-call
locations in sidecars. Default comments remain unchanged. These locations
identify emitted calls; they do not prove original source call sites.

Volt/static-analysis mode writes clean `.lua` source directly and keeps all
upvalue metadata in hidden sidecars. The Volt export manifest is authoritative,
so source fallbacks are copied cleanly and never interpreted as bytecode:

```sh
luau-lifter decompile-folder ./dump ./out \
  --output-extension lua \
  --emit-upvalue-analysis \
  --export-manifest ./dump/.volt-export-manifest.json
```

Analysis output lives under `./out/.tovek-analysis/`. It includes deterministic
static function/site IDs, ordered zero-based VM upvalue slots, one-based ordinals,
`VAL`/`REF`/`UPVAL` capture chains, final emitted names, and decompiled spans. No
IDs, comments, or metadata are inserted into the decompiled source.
Sidecars are content-addressed and hash-bound by the folder manifest, which is
published last. Before that final publication, Tovek atomically publishes
`.tovek-analysis/source-write-provenance.json`, a hidden generation-bound
recovery snapshot. Its schema-v2 `sources` records bind every normalized output
path to the authoritative SHA-256 and byte length committed by that generation;
`generated_source_paths` remains as compatibility metadata. This makes a user
edit after an interrupted generation distinguishable from Tovek-owned bytes,
including writes completed before a later sidecar or manifest failure. It
carries the same `generation_id` as a successful analysis manifest and, for
Volt exports, the authoritative export-manifest SHA-256. A cross-process
output-generation lock prevents overlapping folder runs from mixing source
files and analysis generations.

Decompile **and** validate every output with Luau's own parser:

```sh
luau-lifter validate-folder ./dump ./out
```

Condition normalization is NaN-safe by default. `--assume-no-nan` permits the
more aggressive rewrite `not (a < b)` → `a >= b` when static proof is unavailable;
use it only when inputs cannot be NaN, because the two forms differ for NaN.

`--synthesize-arithmetic-loops` enables an experimental presentation of exact
4-8-term arithmetic accumulations as finite loops. It is off by default and
labels generated loops as synthesis: the original source may have contained
a written-out expression. Available in single-file and folder modes.

### Web server + executor

Build and start the decompiler server (binds `http://127.0.0.1:3000/decompile`):

```powershell
.\run-server.ps1            # builds the release binary first if missing
.\run-server.ps1 -Build     # force a fresh release rebuild
```

It accepts `POST /decompile` with a base64-encoded bytecode body and an optional
`X-Script-Name` header (used to name the chunk). Load `decompile.client.luau` in your
executor to hook `getgenv().decompile` and drive SynSaveInstance through it.

#### Raw bytecode and batch endpoints

Two extra routes skip per-script overhead — ideal for dumping a whole game in one shot:

| Route | Body | Response |
| --- | --- | --- |
| `POST /decompile` | base64 bytecode (one script) | `text/plain` source |
| `POST /decompile/raw` | **raw** bytecode (one script, no base64) | `text/plain` source |
| `POST /decompile/batch` | **many** scripts in one request | JSON results array |

- **Raw** (`/decompile/raw`): send the bytecode bytes verbatim — no base64 encode/decode.
  Use `Content-Type: application/octet-stream`, the optional `X-Script-Name` header, and an
  optional `X-Encode-Key` header (defaults to `203`).
- **Batch** (`/decompile/batch`): decompile many scripts in one request, in parallel. Two
  encodings, chosen by `Content-Type`:
  - `application/json` — `{ "key": 203, "scripts": [ { "id"?, "script_name"?, "bytecode": "<base64>" } ] }`
  - `application/octet-stream` — the binary **MDB1** framing (raw bytecode, no base64):
    `"MDB1"` magic, `u8` version `1`, `u8` key, two zero bytes, `u32`-LE count, then per entry
    a `u32`-LE-length-prefixed name and a `u32`-LE-length-prefixed bytecode blob (all little-endian).
  - Response: `{ "count", "ok_count", "results": [ { "index", "id"?, "script_name"?, "ok",
    "decompilation"?, "error"? } ] }`, in input order. **One bad script never fails the
    batch** — that item gets `ok:false` + an `error`; only a malformed request framing is a
    `400`.

Load `decompile-batch.client.luau` to pre-walk every script, decompile them all in one batch
(raw by default — flip `USE_RAW` if your executor mangles binary bodies), and drive
SynSaveInstance from the cached results.

---

## Building from source

Tovek uses nightly Rust feature gates and pins a specific toolchain — stable will not build it:

```sh
rustup toolchain install nightly-2026-06-15
cargo +nightly-2026-06-15 build --release --locked -p web-server -p luau-lifter
```

The release profile is tuned for distribution: fat LTO, a single codegen unit, no debug
info, and stripped symbols — maximum runtime speed and the smallest possible binary.
Prebuilt binaries are attached to each [release](https://github.com/Kiet1308/Tovek/releases/latest).

### Worker: build và cấu hình xác thực

Worker cần `worker-build 0.8.7` và chế độ `--panic-unwind` để lỗi của một script
không dừng cả batch. Phiên bản công cụ này gọi `cargo +nightly` khi build lại std,
nên cần cài thêm kênh `nightly` bên cạnh toolchain native được ghim ở trên.

```sh
rustup toolchain install nightly --component rust-src --target wasm32-unknown-unknown
cargo install worker-build --version 0.8.7 --locked
cd luau-worker
worker-build --release --panic-unwind --no-opt
```

Không dùng bản WASM mặc định `panic=abort`; source Worker sẽ từ chối build cấu
hình này. `--no-opt` giữ nguyên exception handling; profile Worker cũng giữ
metadata mà wasm-bindgen cần. Kiểm tra runtime cục bộ từ thư mục gốc:

```sh
npm install --prefix out/worker-runtime --no-audit --no-fund miniflare@5.20260925.0-alpha
node scripts/check_worker_runtime.mjs luau-worker/build out/worker-runtime
```

Các endpoint đọc `AUTH_SECRET` từ Worker secret binding. Trước khi triển khai,
dùng `wrangler secret put AUTH_SECRET` trong `luau-worker` và nhập một giá trị
mới; không tái sử dụng khóa từng được commit. Thiếu binding trả HTTP 503, sai
khóa trả HTTP 403. Khóa cũ vẫn tồn tại trong lịch sử Git cho tới khi quản trị
viên xử lý; xóa literal khỏi source không tự xoay khóa trên deployment đang chạy.

### Kiểm thử hồi quy của đợt deep review

`scripts/check_deep_review.py` biên dịch fixture bằng Luau 0.736, chạy bytecode
gốc bằng VM, decompile, biên dịch lại và so sánh hành vi. Script bao gồm các
fixture C1–C12 trong `_harness/_bugs`, với cả ba mức tối ưu. Các file `.dec.luau`
cũ đã được bỏ vì không được harness kiểm tra và không còn phản ánh kết quả hiện tại.
Kết quả mới được sinh vào `--work`, kèm bytecode và `result.json` từng ca.

```sh
python scripts/check_deep_review.py --compiler /path/to/luau-compile --vm /path/to/benchmark-vm --lifter target/release/luau-lifter --work out/deep-review
```

Manifest phân tích batch dùng `corpus_hash_algorithm: ordered-content-sha256-v2`:
băm đường dẫn theo thứ tự cùng SHA-256 của chính dữ liệu mỗi worker đã đọc.
Giá trị này khác thuật toán corpus cũ; chỉ so sánh hash khi cùng tên thuật toán.

---

## Reproducible decompiler benchmark

`scripts/decompiler_benchmark.py` compares local CLI binaries and the optional
[lua.expert API](https://lua.expert/docs) using identical compiler-produced bytes.
It separates bytecode support, recompilation, tested runtime behavior, source
structure and latency. There is no AI judge or combined winner score.

Build the compiler, AST parser and isolated VM from Luau commit
`c2ec0d4e5ca50796ba174a7565298f59aa572268`:

```sh
cmake -S /path/to/luau -B out/luau-build -DCMAKE_BUILD_TYPE=Release
cmake --build out/luau-build --target Luau.Compile.CLI Luau.Ast.CLI --parallel 4
cmake -S scripts/benchmark_vm -B out/benchmark-vm -DCMAKE_BUILD_TYPE=Release -DLUAU_SOURCE_DIR=/path/to/luau
cmake --build out/benchmark-vm --target benchmark-vm --parallel 4
```

Stage the licensed public repositories using `public_source_roundtrip.py
--checkout` (see its `--help`). Then run the stages below, substituting tool paths
and adding `.exe` on Windows as needed:

```sh
python scripts/decompiler_benchmark.py prepare --out out/comparison --vendor out/vendor --compiler out/luau-build/luau-compile --ast out/luau-build/luau-ast --vm out/benchmark-vm/benchmark-vm
python scripts/decompiler_benchmark.py collect --out out/comparison --native tovek-v2=target/release/luau-lifter --native beta-v09=/path/to/beta-v0.9 --online
python scripts/decompiler_benchmark.py evaluate --out out/comparison
python scripts/decompiler_benchmark.py timing --out out/comparison --native tovek-v2=target/release/luau-lifter --native beta-v09=/path/to/beta-v0.9 --online
python scripts/decompiler_benchmark.py report --out out/comparison
```

Open `out/comparison/index.html` to inspect every outcome and compare untouched
outputs against the original source. The frozen plan, binary hashes, HTTP
receipts and raw timing attempts remain beside the report. Collection resumes
from verified responses; use a new directory for a new provider snapshot.

Only `collect --online` and `timing --online` contact lua.expert, at 120 requests
per minute by default. They upload the benchmark's owned fixtures, generated
programs and pinned public sources; private dumps are not part of this corpus.
CI runs offline controls only. Reports and collected outputs stay under ignored
`out/`; no model downloads or AI features are enabled.

The default plan covers 41 development regressions, all 171 selected public
files and 24 fresh generated seeds with alpha-renamed variants, across v9/v12
and optimization/debug profiles. Six capability probes are excluded from quality
scores. Existing fixtures and generator grammar have Tovek development exposure;
fresh seeds are not an independent language-family holdout. Public Roblox modules
receive syntax/structural checks, not full experience execution. Runtime checks
prove only the supplied observations. Profile variants are clustered by original
program/seed for paired uncertainty estimates. API latency includes the network;
CLI latency includes process startup, so their ratio is not engine throughput.

---

## Community

Questions, bug reports, or want to follow development? **[Join the Tovek Discord](https://discord.gg/phY6VUDSF7).**

---

## Credits

Tovek stands on the work of the original **medal** decompiler. All credit for the
foundation goes, in honour and memory, to:

- **Jujhar Singh** (KowalskiFX)
- **Mathias Pedersen** (Costomality)

Keep the Singh and Pedersen families in your prayers. We love you both.

---

## License

MIT — see [LICENSE.txt](LICENSE.txt). © 2024 Jujhar Singh, Mathias Pedersen.
