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
| C4 | remove a by-ref upvalue cell's trivial loop phi (SCOPED to cells) | `cfg/ssa/construct.rs` | repro `state` 15; harness 28→11; AuraUI intact |
| C5 | wrap a trailing multret `Select` in `(…)` in return position | `ast/formatter.rs` | repro; faithful `return (call())` |
| C6 | materialize a loop-mutated by-value capture as a per-iteration snapshot | `ast/materialize_value_captures.rs` (NEW) | repro 1,2,3; harness 11→5; AuraUI intact |
| C7 | don't collapse a return-diamond whose arm is a multret tail | `cfg/ssa/structuring.rs` | repro tcount 3 |
| C8 | `for…do break end` no longer drops the whole function | `restructure/loop.rs`, `luau-lifter/lifter.rs` | repro O0/O1/O2 |
| C9 | inliner closes the side-effect window on a group-write skip | `cfg/ssa/inline.rs` | repro order A,B7; corpus byte-identical |
| C10 | window-aware: keep a captured snapshot only across an intervening call | `ast/inline_temps.rs`, `ast/copy_cleanup.rs` | repro `1`; +1 regression test |
| C11 | keep an effect-free condition/binding that can RAISE | `ast/side_effects.rs`, `cfg/ssa/structuring.rs`, `restructure/jump.rs`, `cfg/ssa/inline.rs` | repro `false` |
| C12 | keep a middle loop's break in deeply-nested multi-break loops | `restructure/loop.rs` | repro count 18; corpus byte-identical |
| C13 | recover a dropped connection write to its captured cell | `ast/recover_dropped_connection.rs` (NEW) | HangingPlacement `local _ = …Connect` → `v22 = …`; 1 corpus file; 0 DECFAIL |
| L1 | LOADB C>1 wires the correct (unsigned I+1+C) CFG edge, no panic | `luau-lifter/lifter.rs` | corpus byte-identical |
| L2 | LOADKX lifts (was `unreachable!` aborting the proto) | `luau-lifter/lifter.rs` | corpus byte-identical |
| L3 | CMPPROTO preserves its `d` jump edge (two-way conditional when D≠0) | `luau-lifter/lifter.rs` | corpus byte-identical; 13/13 repro |
| L4 | non-JUMPX E-form op degrades to a comment, not a panic | `luau-lifter/lifter.rs` | corpus byte-identical |
| L5 | NATIVECALL decoded as ABC (was AD, scrambling A/B/C) | `luau-lifter/instruction.rs`, `lifter.rs` | corpus byte-identical |
| L6 | string constant index 0 decodes to "" instead of underflow panic | `luau-lifter/lifter.rs` | corpus byte-identical |
| (extra) | keyed multret table entry stays truncated when its `[i]=` key is dropped | `ast/formatter.rs` | gen2 repro 3/3; corpus byte-identical |

The whole-program differential harness went from **63 → 4 mismatches** with
**0 decompile failures**; the v9 corpus stays **275/275 parseable**. The 4
residual mismatches are gen2 adversarial stress-tests, **NONE a documented C/L
finding**, each a deep separate root diagnosed but not yet fixed:
- `coro_closure_upval_mutate`, `coro_upvalue_after_loop` — O2-only; aggressive
  coroutine+upvalue optimization the lifter mishandles (correct at O0/O1).
- `nestedctrl_while_with_inner_for_break` — restructurer falls back to `goto`
  (invalid Luau) on a contrived while-with-inner-for-break (C12-adjacent).
- `upval-stress-shared-upval-two-loops` — a snapshot `local mid = total` of a
  by-ref upvalue cell is dropped when the mutation is a LOOP (kept correctly for a
  direct call): a cross-block side-effect gap in the SSA inliner/coalescer.

## Cluster notes — C4/C6 fixed at the root, C13 verified a non-bug

C4 and C6 each took several attempts (documented below); C13 was investigated to a
bytecode-level proof that it is a misdiagnosis. Every subagent finding was verified,
not trusted.

- **C4** — NOW FIXED (see table above). The winning approach: keep the
  self-back-edge exclusion in `remove_unnecessary_params` but SCOPE it to upvalue-
  cell loop phis only (detected by an incoming arg being a grouped cell version),
  so the non-upvalue loop phis the restructurer needs are untouched and AuraUI
  stays valid. Two earlier approaches regressed first: unscoped phi removal (AuraUI
  207 gotos) and relaxing the `coalesce_copies` same-group guard (49 mismatches +
  6 crashes — same-group cell versions can interfere).

- **C6** — NOW FIXED (see table above). After FOUR SSA-level attempts all regressed
  (register conflation / AuraUI-invalid / clobbering out-of-SSA write-backs), the
  winning approach abandons SSA coalescing-avoidance entirely and fixes it at the
  AST level: `materialize_value_captures` re-introduces the lost per-iteration
  snapshot (`local snap = L; closure reads snap`) for a `Copy` capture of a
  loop-mutated local. A value capture IS a snapshot, so it is exact; loop-scoped so
  stable upvalues are untouched; and `snap` is itself captured so inline/cleanup
  leave it. Harness 11→5, DECFAIL=0, closures-upval eliminated, AuraUI intact.

- **C13** — REAL and NOW FIXED (see table). I first mis-called it a "misdiagnosis"
  — that was an ERROR; corrected. Proven real at the bytecode level: HangingPlacement
  func 57 has `MOVE`s writing the `:Connect` result into captured registers, which
  the SSA drops (`local _ = sig:Connect(function() … cell:Disconnect() … end)`, cell
  left nil → self-disconnect never fires). SSA-level unification is blocked by the
  one-register→one-`old_local` conflation (every SSA fix attempt regressed), so it is
  recovered faithfully at the AST level: a dead `local _ = <call>` whose argument
  closure ref-captures + `:Disconnect()`s exactly ONE cell with no non-nil assignment
  is re-targeted to `cell = <call>`. Exactly 1 corpus file changes; 0 DECFAIL.

- **L3 (CMPPROTO), L5 (NATIVECALL)** — NOW FIXED (see table). Both are runtime-only
  JIT pseudo-ops never present in serialized v9 bytecode, so the fixes are correct
  hardening with the 275-file corpus BYTE-IDENTICAL: L5 decodes ordinal 62 as the
  ABC-form call it is (was AD, scrambling A/B/C) and lifts it as a FASTCALL-style
  no-op; L3 emits a genuine two-way conditional preserving the `d` edge when D≠0
  (D==0 keeps the exact fall-through). **set_list multret tail** (#21) is "plausible,
  NO trigger constructed" — not a confirmed finding; the common multret-tail-in-
  constructor case was tested and decompiles correctly (`{1,2,multi()}` → 5 values).

---

## C13 (self-update facet) — dead `local _ = v + 1` → `v += 1` — 2026-06-21

Branch `fix/c13-dead-register-writes` (off `main` @ `0c1ee12`). A user bug report
flagged three faces of the same family ("an assignment to a still-live/captured
local collapsed to a throwaway `local _ = expr`, losing the write"):
1. nested mutual-disconnect connections (cell stays `nil`, `nil:Disconnect()`);
2. **counter self-update** `local _ = v4 + 1` (should be `v4 += 1`);
3. captured boolean flag `local _ = true` (should be `flag = true`).

**Process (CLAUDE.md):** a 7-agent research+review Workflow (5 research + 2
adversarial review) mapped the root cause; I re-verified every finding against the
code/binary; implemented; a 4-agent review Workflow (3 code + 1 perf, read-only
`Explore` agents so they could not touch the uncommitted tree) signed off; a final
summary agent confirmed READY-TO-COMMIT. Every subagent claim was re-verified by
me — including walking back two of my own earlier wrong instincts (a broad
"coalesce any dead write into the register's live class" rule, and a captured-cell
"GATE B" — both removed after they were proven to regress or to be inert).

**Root cause.** Out-of-SSA destruction (`cfg/src/ssa/destruct.rs`) builds final
locals ONLY from copy/phi/upvalue coalescing. A DEAD non-copy self-update
(`v_k = v_j + 1`) whose result no surviving statement reads becomes a singleton
congruence class → a fresh local → named `_` by `name_locals` (Arc count 1). In a
loop the loop-header phi (`coalesce_params`) rescues it, which is exactly why the
same increment is correct `v4 += 1` inside loops but dropped to `local _ = v4 + 1`
on a straight-line / dead-on-path branch (corpus: `WeatherClient/MeteorShower.lua`).

**Fix.** New pass `coalesce_dead_register_writes` (in `destruct()`, after
`coalesce_copies`, before `build_local_map`). It threads the SSA-version→original-
register map (`local_to_group`, already computed in both lifter callers) into
`Destructor::new`. For a dead `dst = rhs` whose RHS reads exactly ONE *distinct*
same-register version (`local_to_group` equality — the lifter's one-register→one-
old_local invariant, i.e. the compiler's `ADD Rn Rn …`; `v = v + v` dedups to one
operand), it merges `dst` into that operand's congruence class IFF the EXISTING
Boissinot interference test (`check_interfere`) permits — the same criterion the
copy/phi coalescers already enforce, so no new unsoundness. `build_local_map`/
`local_declarations`/the formatter then render `v += 1` for free (the reassignment
reads its own LHS). The construct-time `local_to_group` snapshot stays valid for
every surviving version (the post-construct fixpoint only remaps version→version of
the SAME register and mints no register-less version); a stale/missing key
conservatively SKIPS — never mis-merges.

**Why the same-register-read gate is the whole game.** It keeps the change
surgical and is what makes it sound: a discarded length/field read (`local _ = #v2`
— `v2` is a *different* register than the fresh result), a discarded call, an
unused source local (`local _ = Enum.X`, reads no local), and a recovered
`local function` reused on a register later holding a different live variable (the
closure RHS reads no local of its OWN register) ALL fail the gate and are preserved
byte-for-byte. An early broad rule corrupted exactly those recovered-`local
function` cases (PlantReplicator/PotReplicator/Toolbar) — the self-update gate
fixes that and leaves them identical.

**Result.** Corpus byte-diff = exactly ONE file (`MeteorShower.luau`: two
`local _ = v4 + 1` → `v4 += 1`). Corpus-wide `local _ =` 24→22, `+= 1` 274→276 (only
the two intended sites move; the other 22 legitimate `local _ =` survivors
untouched). Deterministic (two runs byte-identical). `validate-folder`:
VALID=262 INVALID=0 scope-files=0 regression-fail=0. Tests: cfg 380, ast 18,
luau-lifter all green + new `c13_dead_increment.rs` regression (v11 fixture
`c13_counter_self_update.luaubc`, asserts no `local _ =` and exactly three `+= 1`).
Perf below the noise floor (a single O(statements) read-only scan; gated out when
`local_to_group` is empty).

**Facets 1 & 3 NOT fixed here (honest scope).** The counter facet reproduces from
stock `luau-compile` v11 and is fixed. The connection and flag facets do NOT
reproduce from the public v11 toolchain — they need the v9-real-bytecode SSA shape
(consistent with FINDINGS.md:128). The one corpus flag instance (`LoadingScript`)
was proven (via instrumentation) to be a **de-inline artifact**: the dropped write
gets a fresh local fully disconnected from the captured cell's register, so NO
SSA-level coalescing can reach it; fixing it belongs in the de-inline / value-
capture pass (the high-risk C6 area) and is not safely doable without the user's
actual bytecode + a deterministic repro. The nested-mutual connection facet has
zero corpus instances and the single-cell form is already handled by
`recover_dropped_connection`. These remain open pending real v9 samples.
