# Changelog

## v0.7.0 — HugeUpgrade P0–P5

Tovek 0.7 is the largest correctness and readability upgrade so far. It rebuilds the decompiler pipeline from SSA destruction through final AST cleanup, with every transformation gated for Luau semantics, capture safety, side-effect ordering, multi-value behavior, NaN behavior, and deterministic output.

### Correctness foundation (P0)

- Reworked SSA construction/destruction for mutually recursive phi nodes, nested loops, and upvalue cells.
- Preserved loop-carried and closure-captured values without stale snapshots, accidental globals, or merged bindings.
- Added materialization for by-value captures when optimizer coalescing would otherwise change closure behavior.
- Fixed local declaration placement, parallel-copy handling, dropped connection captures, and nil-store cleanup.
- Preserved multi-return truncation and call/select semantics across assignments, returns, tables, and inlining.
- Made boolean/relational normalization NaN-safe by default; unsafe relational complement rewrites require the explicit `--assume-no-nan` option.
- Kept potentially throwing or metamethod-observable evaluations instead of treating `!has_side_effects()` as sufficient proof for deletion.

### Structural decompilation (P1)

- Added statement and expression de-inlining for helpers optimized away by Luau `-O2`.
- Recovered strict terminal helpers, guard/value helpers, CPS-style continuations, and repeated terminal regions.
- Added alpha-equivalent matching, parameter binding checks, scope/capture gates, and exact return/loop-control checks.
- Factored common branch tails and rebuilt concise helper calls without moving calls or captured reads across observable effects.
- Added expression-size budgeting so recovery improves readability without creating giant inline expressions.

### Naming and source recovery (P2)

- Greatly expanded local, parameter, callback, service, module, collection, state, result, predicate, event, and Roblox API naming.
- Added interprocedural parameter hints from consistent call sites and stronger conflict rejection when evidence disagrees.
- Preserved service and `require` handles, module-table names, class-like tables, connection collections, React props/children, time values, and common Roblox instance roles.
- Removed the old 32-character identifier truncation; long debug names such as `createAnimationFromKeyframeSequence` now round-trip intact.
- Improved script/module hints, including `init` modules and hyphenated filenames such as `exp-orb` → `ExpOrb`.
- Added file-wide and lexical shadowing protection so inferred or synthesized names never change global/local resolution.

### Control-flow reconstruction (P3)

- Canonicalized adjacent return guards into `elseif` chains and recovered structured ancestor walks, loop exits, and repeat-style loops.
- Recovered guard `continue` forms while respecting loop ownership, repeat-loop semantics, gotos, and complexity caps.
- Flattened safe guard branches, removed unreachable suffixes after unconditional transfers, and retained labels when a goto can enter the suffix.
- Added truth-table-checked condition simplification, safe De Morgan transforms, idempotent `and`/`or` cleanup, and explicit NaN proofs.
- Recovered compound assignments and cleaner left-associated boolean/concatenation shapes without duplicating impure receivers or keys.

### Table and UI reconstruction (P4)

- Rebuilt exploded table literals, computed keys, placeholder fields, and nested declarative UI trees from leaf to root.
- Recovered React-style `props`, `children`, callbacks, event maps, and `createElement` argument trees while preserving evaluation order.
- Added call-receiver materialization for property assignments that would otherwise become unreadable or change call count.
- Rebalanced oversized truthy-selection and concatenation expressions into readable statements with register-pressure gates.
- Preserved named/class/module tables when literal inlining would erase useful source structure.

### Final cleanup and idioms (P5)

- Added capture-aware boolean dataflow with branch joins, loop/repeat backedge invalidation, goto refusal, and current-function upvalue protection.
- Replaced repeated whole-tree dead-store rescans with a near-linear dependency worklist using strict `is_total_pure` deletion proof.
- Extended local-copy cleanup to stable captured destinations while protecting enclosing continuations, loop snapshots, and later source writes.
- Removed redundant trailing void returns at chunk/function tails and restored `break` when a void return only exits a function-tail loop.
- Canonicalized exact Roblox constants: `Vector2.zero/one`, `Vector3.zero/one`, and `CFrame.identity`.
- Recovered named module shapes for function-heavy returned tables.
- Kept callback properties such as `OnClientInvoke`, `OnServerInvoke`, and `OnIncomingMessage` in assignment-function form.
- Preserved discarded calls and potentially throwing arithmetic/index evaluations; readability cleanup never swallows a runtime error.

### Performance and determinism

- Added per-pass profiling and kept hot cleanup paths allocation-conscious.
- Dead-store cleanup is linear in the current function AST and dependency edges, including deeply nested closure trees.
- Folder decompilation remains parallel and deterministic; repeated release runs produce byte-identical output.
- Full release corpus: **262/262 valid**, 13 empty-bytecode inputs skipped, **0** decode/decompile failures, **0** goto files, **0** scope warnings, and **0** regression failures.
- Final corpus size: **114,705 output lines**, 450 fewer than the P4 baseline while retaining evaluations required for exact behavior.

### Verification

- 531 AST tests passed.
- 25 CFG/SSA tests passed.
- 17 lifter and integration tests passed.
- `cargo fmt --check` and `git diff --check` passed.
- Two independent release corpus runs produced matching SHA-256 hashes for all 262 generated files.
