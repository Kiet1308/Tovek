# Source-like structurer completion — 2026-09-10

The roadmap's remaining C4, C6, D and F work is complete in the working tree
based on `80c86c2`. The legacy matcher and its callers have been removed.

## Final evidence

| Check | Result |
|---|---|
| Standard release build | Pass, pinned Rust, fat LTO |
| Workspace all-target tests | 854 passed |
| Python audit/gate tests | 10 passed |
| Strict corpus | 3,936 decompiled, 42 empty inputs skipped, 0 failed |
| Final unsupported results | 317 → 0 across 26,391 structuring invocations |
| Unsafe results | 0 |
| Official bytecode recompile | 3,936/3,936 nonempty inputs |
| Semantic execution at O0/O1/O2 | 45/45 |
| Corpus, residual and semantic bytecode baseline gates | Pass; 0 per-file regressions |
| Public output-size gate | 52 files, 0 regressions |
| Internal control markers in all 3,978 outputs | 0 |
| Output lines / restored inline sites | 504,400 / 660 |
| Write.luau | 1,820 → 959 lines; 6 → 97 restored sites |
| Late SETLIST fallbacks | 59 → 22 |

Nine corpus invocations need the existing preprocessing retry; all nine are
accepted by the proof builder. They do not call legacy code. The inventory
counts final rejection after retry, not provisional first-attempt failures.

The bytecode oracle improves from 2,744 to 2,699 non-equivalent prototypes,
with `investigate` 38 → 14 and `suspect` 6 → 5. Ten local increases were reviewed
individually before refreshing the baseline; the source comparisons, raw deltas
and reasons are in [the oracle review](bytecode_roundtrip/review_20260910.md).
The corpus check recompiles source; it does not execute Roblox services.

## Proof boundaries

- Numeric loops include body-dominated terminal arms and marker pairs without
  a natural backedge. An explicit initial counter is distinct from the public
  result where the single-step shape proves this separation.
- While discovery accepts inverted body edges and whole-header natural cycles.
  Header effects run before every test, including exhaustion. Numeric header
  copies are emitted on their original body and exhaustion paths.
- Terminal fringes remain inside a region only when their entries are owned by
  it. Implicit function exits emit returns. A conditional join must be reachable
  from both arms within the same iteration, before revisiting the header.
- Generic and numeric loops can belong to an outer re-entry cycle, including
  infinite cycles with no function-exit post-dominator. Re-entry tails preserve
  reset statements, normal exhaustion and explicit termination separately.
- Generic results used after the loop retain the original outer cell when it
  is a parameter, captured binding or later write target. A fresh private loop
  binding exports on break/exhaustion; callbacks continue to capture the original
  cell. Incoming/unowned protected cells still fail the proof.
- `close_provenance` records ref-capture open/close state before CLOSEUPVALS is
  erased. Every backedge/continue/break must close the correct register; use,
  write or recapture after close is refused. Certificates intersect and
  obligations accumulate when locals merge. Tests include original bytecode
  mutated to remove or misaddress its close instruction.

The deleted legacy files are `restructure/src/conditional.rs`, `jump.rs` and
`loop.rs`; `restructure/src/lib.rs` now exports the proof/fallback modules.
Luau and Lua 5.1 callers use the proof builder. Unproved inputs retain typed
refusal/certified fallback behavior rather than using the removed matcher.

## Readability and size

Early common-tail factoring precedes `LocalDeclarer`. AST local coalescing
measures pressure in lexical scopes and only merges compatible branch/loop
scopes. This keeps helper temporaries local to their branch and recovers the
additional 91 sites in `Write`. The SSA destructor's copy-coalescing policy is
unchanged.

[UI tree rebuilding](ui_tree_rebuild.md) joins single-use curried factories,
dynamic-key aliases, property writes and late SETLIST tails under evaluation
order, capture and arity checks. Captured table cells retain observable
initialization. Counted fallback loops preserve nil overwrites. Guard flattening
does not duplicate a whole table or closure hidden inside one return statement.

The CI size gate measures lines and bytes for each fixture output independently.
It rejects missing/new outputs and growth beyond the existing tolerance. A
Transform-like 470 → 5,195 line regression cannot be hidden by other files
shrinking. The final 52-file baseline includes the new and expanded source
fixtures; all older fixture outputs passed before the refresh.

## Inventories and reproduction

[initial_317.json](structurer_inventory/initial_317.json) records the starting
231-file inventory. [remaining_corpus.json](structurer_inventory/remaining_corpus.json)
and [remaining_semantic.json](structurer_inventory/remaining_semantic.json) are
both empty after the final standard-release audit. Prototype IDs are local to
each input; inline recovery may visit a prototype more than once.

```powershell
cargo +nightly-2024-12-15 test --workspace --all-targets
cargo +nightly-2024-12-15 build --release -p luau-lifter
$env:MEDAL_DEBUG_RESTRUCTURE = '1'
target/release/luau-lifter.exe decompile-folder D:/Medal/examplebytecode/RobloxProject out/roadmap-audit --key 203 --threads 1 --strict-no-synthetic-control --verbose > out/roadmap-audit.log 2>&1
Remove-Item Env:MEDAL_DEBUG_RESTRUCTURE
python scripts/structurer_inventory.py out/roadmap-audit.log --report out/roadmap-inventory.json

python scripts/semantic_roundtrip.py --compiler D:/Medal/luau-tools-src/build/luau-compile.exe --luau D:/Medal/luau-tools-src/build/luau.exe --lifter target/release/luau-lifter.exe --keep out/semantic-audit
python scripts/bytecode_roundtrip.py --lifter target/release/luau-lifter.exe --compiler D:/Medal/luau-tools-src/build/luau-compile.exe --corpus D:/Medal/examplebytecode/RobloxProject --key 203 --threads 8 --report out/roadmap-oracle.json --baseline docs/bytecode_roundtrip/baseline_corpus.json
```

Final local reports use `out/roadmap-final-*`: corpus inventory, full oracle,
semantic execution/proof, residual default/strict output, workspace tests,
release build and size gate. Generated reports remain ignored by Git.
