# Tovek

**A high-readability, high-performance Luau decompiler.** `beta 0.1`

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
| Compiler `-O2` artifacts | left inlined | de-inlined: single-use temps, expressions, table literals rebuilt |
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
  inlined expressions, and exploded table constructors are reassembled, with the original
  inlining points left as unobtrusive trailing comments.
- **Idiomatic cleanup.** Compound assignments, backtick string interpolation,
  left-associated `and`/`or` (far fewer redundant parentheses), atomic `math.pi`, dropped
  needless `\'` escapes, and removal of redundant local copies.
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

### Web server + executor

Build and start the decompiler server (binds `http://127.0.0.1:3000/decompile`):

```powershell
.\run-server.ps1            # builds the release binary first if missing
.\run-server.ps1 -Build     # force a fresh release rebuild
```

It accepts `POST /decompile` with a base64-encoded bytecode body and an optional
`X-Script-Name` header (used to name the chunk). Load `decompile.client.luau` in your
executor to hook `getgenv().decompile` and drive SynSaveInstance through it.

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

## Credits

Tovek stands on the work of the original **medal** decompiler. All credit for the
foundation goes, in honour and memory, to:

- **Jujhar Singh** (KowalskiFX)
- **Mathias Pedersen** (Costomality)

Keep the Singh and Pedersen families in your prayers. We love you both.

---

## License

MIT — see [LICENSE.txt](LICENSE.txt). © 2024 Jujhar Singh, Mathias Pedersen.
