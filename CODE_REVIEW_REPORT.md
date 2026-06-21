# Tovek / medal decompiler — correctness audit report

**Scope:** find bugs that make the decompiler emit **semantically wrong** Luau (output whose runtime
behaviour differs from the program the bytecode came from). Readability is out of scope.
**Target:** `review/code-audit` worktree, branched from `main` @ `1b8614e`. Binary used for reproduction:
`D:/Medal/medal-decompiler/target/release/luau-lifter.exe` (same HEAD).
**Date:** 2026-06-20.

---

## 1. Method

Two independent thrusts, cross-checked against each other:

1. **Differential fuzzing (dynamic, ground truth).** A self-contained oracle that needs no Roblox:
   `source --(luau-compile -O{0,1,2})--> v11 bytecode --(luau-lifter)--> Luau --(luau.exe)--> stdout`.
   If the decompiled program prints something different from the original, the decompiler changed the
   program's meaning. **863 adversarial programs** were generated across two waves — 618 broad (20 feature
   categories) + 245 targeted (coroutines, buffer, string.pack, table libs, nested control flow, table-key
   edges, and stress variants of every confirmed family) — run at three optimisation levels. Harness:
   `_harness/diff.sh`; triage with path/line normalisation: `_harness/triage.py` / `triage2.py`.
2. **Static review (18 subagents)** over the correctness-critical code (lifter opcode semantics, SSA
   construct/inline/destruct, restructure, and every AST transform). Each finding had to come with a
   reasoned mechanism and, where possible, a trigger program.

**Every** finding below was personally re-verified by compiling + decompiling + running the repro and
diffing output — no subagent claim was accepted on trust. Several plausible-looking static findings were
**refuted** this way (see §5), and one I nearly dismissed turned out real only in a narrower form (C11).

Important characterisation of the oracle: `luau-compile` emits **v11** bytecode; the real Roblox corpus is
**v9**. The dynamic oracle therefore exercises the lifter's opcode semantics + **all** AST/SSA/restructure
transforms (where almost all correctness risk lives), but **not** the v9-specific decode path. v9 decode +
the real corpus were covered statically and by a crash/parse sweep (§4).

---

## 2. Headline

- **13 distinct confirmed correctness bugs** (C1–C13; C13 added from a user report and confirmed in the v9
  corpus). **10 are HIGH severity**
  (wrong values, wrong control flow, dropped output, or lost errors). Most reproduce at **all** optimisation
  levels, i.e. they live in the core lifter / SSA / restructure path, not the optional readability passes.
- **6 verified lifter-level defects** (L1–L6) reachable mainly via hand-crafted / obfuscated bytecode —
  **relevant**, because deobfuscating Roblox bytecode is this tool's actual job.
- The real v9 corpus (275 files) decompiles with **0 crashes** and **100% parseable** output; the bugs are
  about *meaning*, not *validity*.
- Evidence base: 863 differential programs across two waves (618 broad + 245 targeted) at 3 optimisation
  levels, plus 18 static reviewers; **every** confirmed bug personally re-verified by running its repro.

| Recurring theme | Bugs |
|---|---|
| Out-of-SSA / loop-carried value sequencing | C3, C4, C6, C9, C10 |
| Table constructor key handling | C2, C2b |
| Multi-value (multret) truncation | C5, C7 |
| Unsound condition rewriting | C1, C11 |
| Control-flow reconstruction (crash / dropped break) | C8, C12 |
| Dropped write / dead-store misclassification | C13 |

---

## 3. Confirmed correctness bugs

Severity legend: **HIGH** = silently wrong result / control flow / lost output. **MEDIUM** = wrong only on
specific value classes (NaN, type-error operands).

### C1 — `not (a < b)` rewritten to the NaN-unsound `a >= b`  · MEDIUM · `ast/src/unary.rs:88-139`
`Reduce::reduce` rewrites `not(a<b)→a>=b`, `not(a<=b)→a>b`, `not(a>b)→a<=b`, `not(a>=b)→a<b`. For NaN
operands these are **not** equivalent: `not(nan<1)` is `true`, but `nan>=1` is `false`.
*Repro:* `local n=0/0 print(not (n < 1))` → orig `true`, decompiled `false`.
This was the single most frequent dynamic failure (16 of 31 mismatches). It also changes **control flow**
when the expression is a loop/branch guard (e.g. `while not (x >= nan) ...` runs 3× in the source, 0× decompiled).
The equality flips at lines 140-165 (`not(a==b)→a~=b`) are **safe** and should stay.
*Fix:* only flip ordering relations when both operands are provably non-NaN (e.g. integer-typed); otherwise
keep `not (...)`. (Memory notes this was "kept for readability" — it is nonetheless a correctness bug.)

### C2 — table constructor: keyed `[i]=` and positional entries reordered  · HIGH · `ast/src/rebuild_table_literals.rs:145`
`insert_table_entry` only de-duplicates against the first `initial_len` entries, so a keyed `[1]=` and a
positional entry (which also has key 1) coexist as duplicates; the formatter then renders both as array slots.
*Repro:* `local u={[1]=11,[2]=22,"a","b"} print(u[1],u[2])` → orig `a  b`, decompiled `{11,22,"a","b"}` → `11  22`.
Wrong values **and** wrong `#u`. (Variants `key-before-positional`, `positional-then-key`, `nested` all reproduce.)

### C2b — formatter drops non-positive / fractional numeric keys (saturating cast)  · HIGH · `ast/src/formatter.rs:1252`
`are_table_keys_sequential` decides keys are "1,2,3,…" with `(x - 1f64) as usize == i`. Rust's `f64 as usize`
**saturates** (negatives → 0) and **truncates** (fractions toward 0), so `[0]`, `[-1]`, `[0.5]`, `[1.5]`,
`[2.5]` are all judged sequential → key dropped → value relocated into the array part. **Also affects direct
table literals**, not just rebuilt ones.
*Repro:* `local t={} t[0]="zero" print(t[0],t[1],#t)` → orig `zero  nil  0`, decompiled `{ "zero" }` → `nil  zero  1`.
Likewise `u[1.5]="frac"` and `{ "x", [0]="y" }`.
*Fix:* `*x == (i as f64) + 1.0` (reject non-integral / out-of-range), never cast through `usize`.

### C3 — loop-carried parallel assignment sequentialised in an interfering order  · HIGH · `cfg/src/ssa/destruct.rs:171-221`
A parallel assignment `a, b = <…>, <…>` whose targets are loop-carried is lowered to sequential copies in an
order where an earlier write clobbers a value a later copy still needs — the textbook lost-copy / swap problem.
**Canonical minimal repro (`_harness/gen2/loopcarry__loopcarry_swap_advance.luau`):**
```lua
local x, y = 0, 1
for _ = 1, 10 do x, y = y, x + y; print(x) end   -- Fibonacci
```
decompiles to `v = v2 ; v2 = v + v2` — i.e. `x = y` then `y = x + y` using the **new** `x`. The Fibonacci
sequence `1,1,2,3,5,8,…` becomes **powers of two** `1,2,4,8,16,…`. This pattern (coupled recurrences, tuple
swaps `a,b=b,a` inside a loop, running pairs) is extremely common.
*Other forms:* a single multi-return advance `k, v = step(k)` (orig `30` → `50`); `k, v = next(t, k)` hoisted to
the loop top so the body uses this iteration's value and **crashes** on the terminating `nil` (orig `24` →
runtime error). All O-levels. NB a one-shot swap *outside* a loop is handled correctly — the defect is the
loop back-edge copy sequencing.

### C4 — closure-mutated upvalue read with a STALE pre-loop snapshot  · HIGH · `cfg/src/ssa/upvalues.rs:90-116`
A local captured **by reference** and mutated inside a closure that is called in/around a loop: a later read
resolves to a pre-loop SSA version because the call's hidden upvalue write is not modelled as a definition,
and the destructor materialises it as a snapshot `local v2 = v`. Root cause confirmed in code: the
open-upvalue propagation guard `if !visited.contains(&successor)` (line 94) does **not** carry open-upvalue
state across a loop **back-edge** (the loop header is already visited), so a cell opened inside the loop is not
seen as live at the header. The author's own TODO right above it (lines 91-93: *"is there any case where
successor is visited but has open stuff that wasn't already discovered? maybe possible with multiple opens"*)
flags exactly this gap.
*Repro (`gen/inline-bait__inline-bait-helper-mutate-upval.luau`):* iterator mutates upvalue `state`; post-loop
`print(state)` → orig `15`, decompiled `0` (prints the snapshot). 6 dynamic manifestations; all O-levels.
Common signature in output: an unexplained `local v2 = v` just before a loop, read after it.

### C5 — adjust-to-one truncation `(…)` dropped in tail/return position  · HIGH · return/multret lifting + `inline_temps.rs`
`(expr)` truncates a multi-value expression to one value. This adjustment is lost whenever such an expression
ends up in a multret **tail/return** position. Confirmed across a wide range of forms (orig → decompiled
returns *too many* values):
- `return (two())` → `return two()`  (`true 1 nil` → `true 1 2`)
- `return (...)` (vararg) → `return ...`  (`only` → `only dropped gone`)
- `return (table.unpack(t))` / `return (select(2,…))` / `return (string.byte(s,1,3))` — all lose the `(…)`
- **via temp inlining:** `local x = (select(2, ...)) ; return x` → `return select(2, ...)` — `inline_temps`
  inlines a temp whose value was adjusted-to-one into a tail position, dropping the adjustment.
In non-tail positions (`print((two()))`, `local a = (two())` that is *not* re-inlined) the truncation IS kept;
the defect is the tail/return context. 8 confirmed manifestations. All O-levels.
*Fix:* preserve a single-value adjustment (e.g. keep the wrapping parens / a `Select`-to-one) when a multret
expression is emitted in a return or last-argument/last-field position; never inline a one-adjusted temp into
a multret tail.

### C6 — per-iteration captured local eliminated; closure rebinds to the shared loop variable  · HIGH · `cfg/src/ssa/upvalues.rs:51-54`
A per-iteration `local x = i` captured **by value** is eliminated and the closure is rewired to capture the
loop variable directly. Each closure must capture a fresh cell; sharing one makes them all read the final value.
Root cause confirmed in code: `UpvaluesOpen::new` tracks only `Upvalue::Ref` captures and explicitly discards
`Upvalue::Copy(_) => None` (line 52), so a by-value (snapshot) capture is never treated as a distinct cell and
collapses onto the shared loop variable.
*Repro (`gen/closures-upval__closures-upval-loopvar-while.luau`):*
```lua
local fns = {}
local i = 1
while i <= 3 do local x = i; fns[i] = function() return x end; i += 1 end
print(fns[1](), fns[2](), fns[3]())   -- orig 1 2 3, decompiled 4 4 4
```
When the captured value is used as an index/offset this also turns into a **runtime crash**: a variant
capturing a buffer offset per iteration decompiles to all closures reading the post-loop offset →
`buffer access out of bounds`.

### C7 — `if c then return a() else return b() end` collapsed to `cond and/or` truncates multret  · HIGH · `ast/src/conditional_expressions.rs:121`
A return-diamond whose arms return multiple values is collapsed into a short-circuit ternary
(`return not p and 9 or a()`). The `and/or` form truncates `a()` to **one** value.
*Repro (`gen/multret-vararg__multret-vararg-conditional-multret.luau`):* `choose(true)` should return `1,2,3`
(`select("#",...)==3`); decompiled returns `1` (`==1`).
*Fix:* never collapse when any arm is a multret tail (Call/MethodCall/VarArg/Select in value position).

### C8 — `for … do break end` (runtime bound) → entire function dropped  · HIGH (data loss) · `restructure/src/loop.rs:92-93`
A for-loop whose body is a single `break` indexes `then_successors[0]` out of bounds; the per-function
`catch_unwind` turns the panic into `-- failed to decompile`, losing the whole function's body.
*Repro (`_harness/_t_fb2.luau`, O0):* `local n=3 for i=1,n do break end print("after")` → orig `after`,
decompiled output is just `-- failed to decompile`. (At O2 the compiler removes the loop, hiding it.)
Also fires for a natural nested pattern — a `while` loop containing a `for` loop with a `break`
(`_harness/gen2/nestedctrl__nestedctrl_while_with_inner_for_break.luau`) → whole function dropped.

### C9 — SSA inliner reorders observable side effects  · HIGH · `cfg/src/ssa/inline.rs:222-229`
Inlining a single-use definition into a later expression can move its call **past** an intervening
side-effecting statement, swapping observable order.
*Repro (`_harness/_t_f10.luau`, O0):* `local c1=A(); local m=B(a); … return c1+m+a` — decompiled computes `B`
before `A`; side-effect log `A,B7` becomes `B7,A`.

### C10 — captured-local snapshot eliminated; upvalue read moved PAST a mutating call  · HIGH · `ast/src/inline_temps.rs` (+ copy_cleanup)
The inverse of C4. `local captured = source` snapshots an upvalue before an opaque call mutates it; the temp
is eliminated and the read is relocated **after** the mutating call. `collect_usage` is per-block, so the
`captured` flag is missed when the capturing closure lives in an enclosing/sibling scope.
*Repro (`_harness/_t_f16.luau`):* `local captured = source; bump(); return captured` → decompiled
`bump(); return source` → orig `1`, decompiled `99`. Reproduces at **O0 and O2**.

### C11 — empty `if` drops a relational comparison that can raise  · MEDIUM · `restructure/src/jump.rs:33-46`
An `if <cmp> then end` whose then-block is empty is dropped on the assumption the condition is side-effect
free. Relational comparisons on type-mismatched operands (two tables, nil, mixed) **raise an error**; dropping
them loses the error. (Comparisons whose operands have side effects ARE preserved — only the
effect-free-but-erroring case is unsound.)
*Repro (`_harness/_t_eif3.luau`):* `if a < b then end` with `a,b={},{}` inside `pcall` → orig `false`
(comparison errors), decompiled `true` (comparison gone).

### C12 — a `break` is dropped when reconstructing complex nested loops  · HIGH · `restructure/src/loop.rs` / break-target resolution
In loops nested ≥3 deep where an inner loop has **multiple distinct break targets** and a middle loop also
breaks, the middle loop's `break` can be **omitted** from the reconstructed source, so that loop runs extra
iterations.
*Repro (`_harness/gen2/nestedctrl__nestedctrl_triple_break_label.luau`):* the `break` after `break-outer-j @ 2,2`
is dropped; the j-loop continues to j=3, emitting an extra `break-j @ 2,3,1` line, count `18` → `19`.
The decompiled source visibly lacks the `break` inside `if i == 2 and i2 == 2 then … end`.
(Simple 2–3 level loops with a single break each reconstruct correctly; the defect needs the multi-break
structure, so it is real but narrow.)

### C13 — assignment to a LIVE local dropped as `local _ = expr` (write lost) · HIGH · SSA "unused result" / dead-store classification · v9 real bytecode
An assignment whose result the decompiler deems unused is emitted as `local _ = expr`, **throwing away the
write** — even though the target local is still live. Two observed facets:
- **(a) closure-captured connection:** a local that is read **only through closure upvalues** (never in
  straight-line code) is judged dead, so `x = signal:Connect(…)` becomes `local _ = signal:Connect(…)`. The
  local stays `nil`, and later `if x then x:Disconnect() end` becomes dead code → the connection is never
  cleaned up (leak / different behaviour). This is the residual of the merged `fix/closure-captured-local`,
  which catches some cases but not all (mutually/self-referencing connections).
- **(b) self-update / guarded default:** `x = x + 1` (counter) or `field = expr` whose result looks unused
  becomes `local _ = expr`, dropping the increment/default-set.

**Confirmed in the real v9 corpus** (current binary), not just reported — `Client/HangingPlacement.client.luau`:
```lua
local v22 = nil                                                   -- :45
local _ = localPlayer.AncestryChanged:Connect(function(_, parent) -- :1449  ❌ should be  v22 = …:Connect(…)
    … end)
if v22 then            -- :1507   v22 is ALWAYS nil → this whole block is dead
    v22:Disconnect()   -- :1508   the AncestryChanged handler is never disconnected
    v22 = nil
end
```
Facet (b) suggestive case: `Client/UI/GiftcodeAdminUI.luau` `if not tonumber(p.totalRequested) then local _ = #v2 + #v3 end`.
*Could not reproduce via stock `luau-compile` (v11)* — the trigger is the v9 pattern of pre-declared `nil`
locals + self/mutually-referencing closure assignments; confirmed instead directly on the v9 corpus.
Same subsystem as C4/C6 (SSA capture handling) but a **distinct** symptom: C4 = stale read, C6 = wrong
capture, **C13 = the write is dropped entirely**.
*Fix direction (from reporter, sound):* never emit `local _ = expr` when (1) the op updates an existing local,
(2) the target is referenced by any nested closure/upvalue, or (3) it matches a self-update `x = x ± …`.

---

## 4. Lifter-level findings (real code; reachable mainly via hand-crafted / obfuscated bytecode)

These do not arise from stock `luau-compile`, but obfuscated Roblox bytecode (the tool's real input) routinely
contains unusual shapes, so they matter. All were verified by reading the cited code.

- **L1** `lifter.rs:313-318` — `LOP_LOADB` with jump offset `C>1` pushes the CFG edge to `index+2` (assumes
  `C==1`); for `C>1` it wires control to the wrong block, or `block_to_node().unwrap()` panics. `discover_blocks`
  (line 140) computes the correct `index+1+C`, so the two disagree. Stock compiler always emits `C==1`.
- **L2** `lifter.rs:808` — `LOP_LOADKX` is listed among aux-bearing ops in the deserializer
  (`function.rs:64`) but has **no** lift arm, so it falls to `_ => unreachable!()` and aborts the whole
  function. Stock compiler emits it for a proto with **>32768 constants** (`trig=yes`, but hard to minimise).
- **L3** `lifter.rs:1348` — `LOP_CMPPROTO` with non-zero `D` silently drops the jump edge (lowered as
  fall-through). v11 JIT-guard op, never compiler-emitted.
- **L4** `lifter.rs:1363` — non-`JUMPX` E-form ops (e.g. `LOP_COVERAGE`) hit `unreachable!()` → abort. Only
  appears in coverage-instrumented builds.
- **L5** `instruction.rs:156` — `LOP_NATIVECALL` decoded as AD-form instead of ABC-form.
- **L6** `lifter.rs:1414` — a STRING constant with index 0 does `string_table[v-1]` → underflow panic instead
  of decoding empty/nil.
- *(plausible, no trigger constructed)* `set_list.rs:81-100` — `SetList::Display` fallback truncates a multret
  tail to a single array slot.

---

## 5. Investigated and NOT a bug (honest negatives)

- **Real v9 corpus:** 275 files → 262 decompiled, 0 failed, 0 panics; **275/275 outputs parse** as valid Luau.
  No crash or syntax defect on production input.
- **Categories that passed cleanly** in 618-program fuzzing (no mismatches): number formatting/precision,
  string escapes, string library, operator precedence/associativity, `and/or` short-circuit side-effect order
  (non-loop), metatables/metamethods, varargs/select (non-truncation), generic numeric arithmetic, closures
  in straight-line code.
- **Refuted static claims:** `#22` naming shadow-vs-global (no repro), `#23` goto back-edge `continue` drop
  (no repro), `#25` `math.pi` under a shadowing local `math` (folding removes the conflict before emission),
  `#3` Vector→`Vector3.new` (correct in the Roblox target environment).
- **False positives the harness initially flagged** (NOT bugs): Luau runtime error strings embed the source
  **filename + line**, which legitimately differ between `orig.luau` and `*.dec.luau`; and the generated
  `goto`/`::label::` programs fail because **Luau has no goto** (the *source* doesn't compile). Triage
  normalises these out.

---

## 6. Suggested priority

1. **C3 / C4 / C6 / C9 / C10** — the SSA/loop value-sequencing cluster. These silently corrupt very common
   patterns (loop-carried iterators, module-state closures, per-iteration capture) at **all** optimisation
   levels and produce no visible warning. Highest impact on trust.
2. **C2 / C2b** — table reconstruction; trivial to hit, wrong values + wrong length; C2b is a one-line cast fix.
3. **C5 / C7** — multret truncation in return position.
4. **C8 / C12** — restructure control-flow defects (panic → whole-function loss; dropped `break` → extra
   iterations). C8 is a small structural fix; C12 is narrower (complex multi-break nests).
5. **C11**, **C1** — narrower value classes (type-error operands / NaN); C1 is a known readability trade-off.
6. **L1–L6** — harden the lifter for adversarial bytecode (replace `unreachable!()` with graceful per-op
   fallbacks; fix the LOADB edge target; reject index-0 string constants).

Reproductions for every confirmed bug live under `_harness/` (the `gen/` corpus, the `_t_*.luau` minimal
cases, `diff.sh`, `triage.py`). A condensed log is in `FINDINGS.md`.
