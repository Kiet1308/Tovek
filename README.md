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

It also happens to be a lot faster.

AI features are disabled. The current decompiler uses deterministic rules and
does not load or download models or call AI services. AI is not part of this release. Model weights and model caches
stay local and must not be committed or uploaded to GitHub, including release
assets, workflow artifacts or Git LFS.

---

## Why Tovek over medal?

### Readable output, not just *correct* output

| | medal | **Tovek** |
|---|---|---|
| Local & parameter names | `v1`, `v2`, `v3` … | Inferred from usage — `player`, `connection`, `track`, `dt`, `child`, `color` … |
| Parameter types | dropped | Recovered from the compiler's bytecode type info: `function Api.emitAt(name: string, cframe: CFrame?)` — exact for primitives, `Vector3`, `buffer`, `thread` and tagged host types (`CFrame`, `Color3`, …); the types also name otherwise-anonymous parameters (`cframe`, `vector`, `callback`) |
| Service / module handles | `game:GetService("X")` inlined at every call site | Preserved once as a named header local (`local Players = game:GetService("Players")`) |
| OOP methods | `function T.method(self, ...)` | `function T:method(...)` with real `self` recovery |
| Compiler `-O2` artifacts | left inlined | de-inlined: temps, expressions and UI tables rebuilt; dead branches/discards removed |
| Compound assignment | `x = x + 1` | `x += 1` (including indexed targets) |
| Strings | `string.format("%*", a, b)` | backtick interpolation `` `{a}{b}` `` |
| Boolean / guard chains | raw `and`/`or` spaghetti | normalized conditions, collapsed predicates, `x and x:FindFirstChild(...)` → named |
| Control flow | gotos & guard-`continue` left raw | recovered into structured `if` / loops where sound |

A few of the things Tovek does that upstream medal does not:

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
- **Modern bytecode coverage.** Reads Luau serialization **v4–v13**, including v12
  size-delimited prototypes, 64-bit cost metadata, call feedback and `CALLFB`.
  v12 is covered by 246 compiler/runtime profiles, malformed-input checks and executed
  wasm32 reader tests. v13 double-vector serialization has targeted coverage; v14 is
  unsupported. Runtime-mutated `CMPPROTO` guards are explicitly rejected because their
  prototype-identity predicate cannot be faithfully reconstructed in source. See the
  [v12 validation and limits](https://kiet1308.github.io/Tovek/changelog.html#bytecode-v12).
- **Validated output.** The full regression corpus (262/262 files) re-parses cleanly under
  Luau's own front end (`luau-analyze`), so readability gains never come at the cost of
  producing source that won't parse.

### Substantially faster

- **15× faster on the large v12 regression sample:** 15.296 s → 1.018 s median of
  five interleaved runs, with byte-identical output. Cached binding summaries remove
  the out-of-SSA cross-product scan while retaining source-binding constraints.
  This is a measured sample result, not a speedup claim for every script.
- **~2× faster** on a single file, and up to **32× faster** across a corpus (some files 80×+).
- **mimalloc** global allocator — the decompiler is allocation-bound, and per-thread
  free-lists replace the slow system allocator.
- **Parallel** per-function lifting and parallel folder decompilation (rayon).
- **Deterministic, byte-identical output** regardless of thread count (stable local IDs),
  so results are reproducible and diffable.
- Fixed several pathological blowups in the original (e.g. exponential upvalue handling).

### Better tooling

- A native **`decompile-folder`** subcommand that decompiles an entire SynSaveInstance dump
  in parallel.
- A native **`validate-folder`** subcommand that decompiles *and* validates every output
  against Luau's parser in one pass — replacing a slow shell script (~46× faster).
- A small **HTTP server** (`web-server`) for the executor → server workflow, plus a
  ready-to-use client script.
- A **Cloudflare Worker** target (`luau-worker`) for serverless deployment.

---

## Output and validation

The [V2 release article](https://kiet1308.github.io/Tovek/changelog.html#evaluation)
compares the validated V2 output with v0.9 beta, including readability gains,
runtime checks and remaining regressions. The [interactive examples](https://kiet1308.github.io/Tovek/#output)
illustrate module exports, naming and direct returns.

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
rustup toolchain install nightly-2024-12-15
cargo +nightly-2024-12-15 build --release --locked -p web-server -p luau-lifter
```

The release profile is tuned for distribution: fat LTO, a single codegen unit, no debug
info, and stripped symbols — maximum runtime speed and the smallest possible binary.
Prebuilt binaries are attached to each [release](https://github.com/Kiet1308/Tovek/releases/latest).

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
