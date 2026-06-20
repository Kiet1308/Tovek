# Decompiler correctness fixes — Progress

Branch `fix/decompiler-correctness` (off `main` @ `2160037`). Goal: fix every
confirmed semantic-correctness bug in `CODE_REVIEW_REPORT.md` / `FINDINGS.md`
(C1–C13) plus the lifter-hardening findings (L1–L6), each verified against the
differential harness (`source → luau-compile → luau-lifter → luau.exe`, diff
stdout) and the 275-file v9 corpus (byte-diff + 100% parse).

Process per bug (per CLAUDE.md): deep research + adversarial verification via
parallel Opus subagents (a 21-agent Workflow over the remaining cluster), then I
personally re-verify every finding against the real code before implementing,
rebuild, run the per-bug repro + the full 863-program differential harness +
the corpus byte-diff, and only commit when clean. **Every subagent finding was
verified, not trusted** — two researched fixes (C6, C13) were found to regress /
be unsound and were NOT shipped as proposed.

## Fixed and shipped

| Bug | One-line | Where | Validation |
|---|---|---|---|
| C1 | `not (a<b)` no longer rewritten to NaN-unsound `a>=b` | `ast/unary.rs` | repro + corpus (faithful `not(<)` / preserved guards) |
| C2 | mixed keyed+positional table keeps explicit keys | `ast/formatter.rs` | repro `a b 2` |
| C2b | non-integral/out-of-range numeric keys not dropped (no `usize` cast) | `ast/formatter.rs` | repro `zero nil 0` |
| C3 | loop-carried parallel copy: pre-spill destination-reading RHS | `cfg/ssa/destruct.rs` | repro Fibonacci; corpus byte-identical |
| C5 | wrap a trailing multret `Select` in `(…)` in return position | `ast/formatter.rs` | repro; faithful `return (call())` |
| C7 | don't collapse a return-diamond whose arm is a multret tail | `cfg/ssa/structuring.rs` | repro tcount 3 |
| C8 | `for…do break end` no longer drops the whole function | `restructure/loop.rs`, `luau-lifter/lifter.rs` | repro O0/O1/O2 |
| C9 | inliner closes the side-effect window on a group-write skip | `cfg/ssa/inline.rs` | repro order A,B7; corpus byte-identical |
| C10 | window-aware: keep a captured snapshot only across an intervening call | `ast/inline_temps.rs`, `ast/copy_cleanup.rs` | repro `1`; +1 regression test |
| C11 | keep an effect-free condition/binding that can RAISE | `ast/side_effects.rs`, `cfg/ssa/structuring.rs`, `restructure/jump.rs`, `cfg/ssa/inline.rs` | repro `false` |
| C12 | keep a middle loop's break in deeply-nested multi-break loops | `restructure/loop.rs` | repro count 18; corpus byte-identical |
| L1 | LOADB C>1 wires the correct (unsigned I+1+C) CFG edge, no panic | `luau-lifter/lifter.rs` | corpus byte-identical |
| L2 | LOADKX lifts (was `unreachable!` aborting the proto) | `luau-lifter/lifter.rs` | corpus byte-identical |
| L4 | non-JUMPX E-form op degrades to a comment, not a panic | `luau-lifter/lifter.rs` | corpus byte-identical |
| L6 | string constant index 0 decodes to "" instead of underflow panic | `luau-lifter/lifter.rs` | corpus byte-identical |

The whole-program differential harness went from **63 → ~?? mismatches** with
**0 decompile failures**; the v9 corpus stays **275/275 parseable**.

## Deferred — the SSA capture/sequencing cluster (researched + ATTEMPTED, every fix regressed)

These three share one root and each researched/attempted fix introduced a NEW
bug, so none was shipped (the "no new bugs" requirement is absolute). Each is
documented with the precise root cause pinned during the attempts:

- **C4** (stale by-ref upvalue snapshot after a loop). Fix attempted & briefly
  committed: `remove_unnecessary_params` excludes the self back-edge arg so the
  trivial loop phi `p = phi(x, p)` is removed. It fixed the repro (`state` 15) and
  cut the harness 63→10, but **removing a loop-header phi is incompatible with the
  restructurer on complex CFGs**: `Client/UI/AuraUI.luau` became fully
  unstructurable (207 `goto` fallbacks → invalid Luau, 274/275 parseable).
  Reverted. Needs a phi-PRESERVING approach (open-upvalue back-edge propagation in
  `UpvaluesOpen::new`, or dropping the upvalue-grouped trivial self-copy in
  `coalesce_copies`).

- **C6** (per-iteration by-value capture collapsed onto the loop var). Two
  attempts, both regressed: (1) marking the `Upvalue::Copy` as open in
  `mark_upvalues` groups the WHOLE bytecode register (one register → one
  `old_local`), conflating register-reuse values → 6 closures-upval harness
  programs broke ("attempt to get length of a function value"). (2) A
  conflation-free two-guard version (refuse the copy in `propagate_copies` +
  refuse the coalesce in destruct, keyed by the value-captured local) fixed the
  repro (`1,2,3`) but **preserving the value-capture copy of a table accumulator
  produced loop-carried copies the restructurer mangled into invalid Luau** in
  `AuraUI.luau` (parse failure).

- **C13** (`local _ = expr` drops a live write to a closure-captured / self-updated
  local). The researched phi-passthrough is unsound (it splits a genuine merge and
  force-materializes a `nil` default). The TRUE trigger is register reuse: the
  orphaned write is the connect-WRITE version, a distinct `RcLocal` from the cell
  the closure reads (NEWCLOSURE precedes the assigning CALL), so it is never
  unified into the cell. Needs version-level unification, not a name rename.

Common root: the lifter maps one bytecode register to one `old_local`, so the SSA
upvalue-cell membership is register-granular. A correct fix for the cluster needs
*version-granular* cell membership coherent across `UpvaluesOpen`/`mark_upvalues`
/ `propagate_copies` / `coalesce_copies` AND tolerant of the restructurer — a
larger change that must be validated against the FULL 275-file corpus *parse*
(not just the differential harness, which tests generated programs, not corpus
syntactic validity — the trap C4 fell into).

- **L3 (CMPPROTO), L5 (NATIVECALL)**: runtime-only JIT pseudo-ops that never
  appear in serialized bytecode, so the hardening value is ~0 while L5's decode
  re-form and L3's two-way CFG rewrite carry real corpus-regression risk — not a
  sound trade. **set_list multret tail**: the verify pass found the proposed
  rewrite unsound (a fixed multi-assign cannot express an open multret spread).
