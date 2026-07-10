# Tovek

**A high-readability, high-performance Luau decompiler.** `v0.8`

[**💬 Join the Tovek Discord →**](https://discord.gg/phY6VUDSF7)

Tovek is a fork of the [medal](https://github.com/Stefanuk12/medal-decompiler) Luau
decompiler, rebuilt around a single goal: **output you can actually read.** Where most
decompilers hand you a wall of `v1, v2, v3 …` and inlined compiler noise, Tovek
reconstructs names, methods, control flow and idioms so the result reads close to the
source a human would have written — without sacrificing correctness.

It also happens to be a lot faster.

---

## Why Tovek over medal?

### Readable output, not just *correct* output

| | medal | **Tovek** |
|---|---|---|
| Local & parameter names | `v1`, `v2`, `v3` … | Inferred from usage — `player`, `connection`, `track`, `dt`, `child`, `color` … |
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
- **Modern bytecode coverage.** Reads every Luau bytecode version up to **v11** — including
  the v10/v11 format extensions (the per-proto feedback vector and the `CALLFB` / `CMPPROTO` /
  class-member opcodes) on top of the **v9** Roblox ships today, plus the previously-missing
  userdata opcodes. (medal stops at version 6 and can't read Roblox at all.)
- **Validated output.** The full regression corpus (262/262 files) re-parses cleanly under
  Luau's own front end (`luau-analyze`), so readability gains never come at the cost of
  producing source that won't parse.

### Substantially faster

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

## How Tovek stacks up against the field

medal is the open-source upstream Tovek forks. **[lua.expert](https://lua.expert/)** — a
closed-source, API-only service — is the strongest *free* decompiler around and the bar most
people actually compare against. We ran the **same code** through all four (Source, medal,
lua.expert, Tovek) and read the output side by side. The
[landing page](https://kiet1308.github.io/Tovek/#duel) lets you flip between them on each
example; every panel is real, unedited output.

A note on medal: it **can't read Roblox's current v9 bytecode at all** (it stops at version 6),
so its column is the same source compiled with standard Luau (`-O2 -g1`) and decompiled — its
raw style is unchanged. lua.expert and Tovek both read the real v9 bytecode directly.

lua.expert is genuinely good — it recovers function names and modern `for` loops. But it stops
where Tovek keeps going:

| | Source | medal | lua.expert | **Tovek** |
|---|---|---|---|---|
| Reads Roblox v9 bytecode | — | **no** | yes | yes |
| Local & parameter names | real | `v1, v_u_3` | `p1, v1` + some | inferred + handle names |
| OOP `:` methods & `self` | yes | `.m(_, …)` | `.m(p1, …)` + colon-call mismatch | `:m(…)`, real `self` |
| Luau `-O2` inlined helpers | — | left inlined | **inlined & duplicated** | **de-inlined + marked** |
| Redundant `x = nil` stores | none | kept | kept | removed |
| `math.huge` / `math.pi` | symbolic | `(1 / 0)` | `(1 / 0)` / raw float | `math.huge` / `math.pi` |
| Dead `if x then true else false` | none | — | **dozens** (47 in one file) | normalized away (0) |
| Compound assignment | `x += 1` | `x = x + 1` | `x = x + 1` | `x += 1` |
| Per-function comment noise | none | `-- upvalues:` | `--[[ name｜Line｜Upvalues ]]` **every fn** | none |
| Tool watermark in output | none | none | `-- https://lua.expert/` every file | none |
| Source / license | — | open | **closed — API only** | **open — MIT** |

Concrete, verified examples:

- **It un-inlines the optimizer.** In `ShovelHighlight`, the compiler inlined `clearHighlight`
  into `updateTarget`. lua.expert copy-pastes the teardown body **7×**, nested four branches
  deep (227-line file); Tovek restores six `clearHighlight(p)` calls and flattens it with
  guard-returns (177 lines — the original is 144).
- **Smart names + no dead stores.** For the inlined `getMainGui` in `InitNpcQuest`, Tovek names
  the result `main` (from `FindFirstChild("Main")`) and drops the dead `= nil` stores. medal and
  lua.expert leave it `v35`/`v1` and write `= nil` in two branches where it is already nil.
- **`math.huge`, not `(1 / 0)`.** In one file Tovek collapses **47** pointless
  `if x then true else false` ternaries to **0** and restores every `(1 / 0)` to `math.huge`.
- **Real methods.** lua.expert emits `function t.DisableCollision(p1, p2)` then calls it
  `t:DisableCollision(v2)` — a dot/colon mismatch that wouldn't round-trip; medal leaks `self`
  as `_`. Tovek recovers the real `function X:DisableCollision(folder)`.
- **It even catches what lua.expert gets *wrong*.** In `ChatTipsClient`, lua.expert folds away
  a captured version snapshot, leaving `t._configVersion == t._configVersion` — always true, so
  a config-reload guard becomes dead code. Tovek keeps the snapshot.

lua.expert keeps a few rational constants (`1/60`) Tovek currently prints as a decimal, and
occasionally guesses a local name Tovek leaves as `v*` — but the wins above are *structural*
(un-inlining, real methods, killed dead ternaries and nil-stores, restored idioms, correctness)
and hold across the whole sample, not one cherry-picked file.

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

Decompile **and** validate every output with Luau's own parser:

```sh
luau-lifter validate-folder ./dump ./out
```

Condition normalization is NaN-safe by default. `--assume-no-nan` permits the
more aggressive rewrite `not (a < b)` → `a >= b` when static proof is unavailable;
use it only when inputs cannot be NaN, because the two forms differ for NaN.

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
cargo +nightly-2024-12-15 build --release -p web-server -p luau-lifter
```

The release profile is tuned for distribution: fat LTO, a single codegen unit, no debug
info, and stripped symbols — maximum runtime speed and the smallest possible binary.
Prebuilt binaries are attached to each [release](../../releases).

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
