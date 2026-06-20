# Decompiler correctness audit — confirmed findings

> ## FIX STATUS (branch `fix/decompiler-correctness`)
> **Shipped & validated (15):** C1, C2, C2b, C3, C5, C7, C8, C9, C10, C11, C12, L1, L2, L4, L6.
> Each: per-bug differential repro PASS, all unit tests green, 275/275 corpus files
> parse, and a full 863-program harness run with no new regression family. A final
> adversarial review found one residual hole (C10 `copy_cleanup` window not seeing
> nested-block reads) — fixed. See `Progress.md`.
>
> **Deferred — NOT shipped because every attempted fix introduced a new bug
> (C4, C6, C13):** the SSA upvalue-cell cluster. The lifter maps one bytecode
> register to one `old_local`, so cell membership is register-granular; the
> researched/attempted fixes either conflated register-reuse versions (C6 v1, broke
> 6 harness programs), made a complex CFG unstructurable → invalid `goto` output
> (C4 broke AuraUI; C6 v2 broke AuraUI), or were unsound (C13 phi-passthrough).
> A correct fix needs *version-granular* cell membership coherent across
> UpvaluesOpen/mark_upvalues/propagate_copies/coalesce_copies AND tolerant of the
> restructurer, validated against the full-corpus PARSE (not just the differential
> harness). Intentionally skipped: L3, L5 (runtime-only JIT ops, never serialized),
> set_list multret-tail (unsound rewrite).

Method: differential harness. source --luau-compile -O{0,1,2}--> v11 bytecode --luau-lifter--> Luau --luau.exe--> output.
A divergence in printed output = confirmed semantic bug. Binary: D:/Medal/medal-decompiler/target/release/luau-lifter.exe @ HEAD 1b8614e.

## CONFIRMED (reproduced via harness)

### C1 — `not (a < b)` family: NaN-unsound relational negation  [severity: medium]
- Location: `ast/src/unary.rs:88-139` (`Reduce::reduce`)
- `not(a<b)`→`a>=b`, `not(a<=b)`→`a>b`, `not(a>b)`→`a<=b`, `not(a>=b)`→`a<b`. Unsound for NaN.
- Repro: `local n=0/0; print(not (n < 1))` → orig `true`, decompiled `n>=1` → `false`. (O0)
- Note: equality flips (`not(a==b)`→`a~=b`) at 140-165 ARE safe. Memory says this was "kept for readability"; still a correctness bug.

### C2 — table constructor: keyed `[i]=` + positional entries reordered  [severity: high]
- Location: `ast/src/rebuild_table_literals.rs:145` (`insert_table_entry`) — dedups only against first `initial_len` entries, so a keyed `[1]=` and a positional entry (key 1) coexist as duplicates; formatter renders both as array slots.
- Repro: `local u={[1]=11,[2]=22,"a","b"}; print(u[1],u[2])` → orig `a b`, decompiled `{11,22,"a","b"}` → `11 22`. (all O)
- Wrong values AND wrong array length (#u).

### C3 — loop with multi-return advance: parallel-copy sequentialized wrong  [severity: HIGH]
- Area: SSA destruct / restructure loop back-translation.
- `k,v = step(k)` (step returns 2 values from old k) decompiled as `v+=1; v2+=v3; v3 = v*10` — second result reads `k` after it was clobbered. Violates simultaneous multiple-assignment.
- Repro `_harness/_m3.luau`: orig `30`, decompiled `50`. (all O)
- Variant (`next`): `k,v=next(t,k)` advance hoisted to loop top → body uses this-iteration value, crashes on terminating nil. `_harness/gen/genfor-iter__genfor-iter-next-explicit.luau`: orig `24`, decompiled errors `add on number and nil`. (all O)
- Single-var equivalents (`i=i+1; v=v+100` as separate stmts) decompile correctly — trigger needs a single multi-value producer feeding both a condition var and a body var.

### C4 — closure-mutated upvalue read with stale pre-loop snapshot  [severity: HIGH]
- Area: SSA construct/destruct (captured-local versioning).
- Iterator closure mutates upvalue `calls`; post-loop `print(calls)` decompiled as `local v2 = v` (pre-loop snapshot) then `print(v2)`.
- Repro `_harness/gen/genfor-iter__genfor-iter-side-effect-count.luau`: orig `4`, decompiled `0`. (all O)

### C5 — `return (f())` adjust-to-one truncation dropped  [severity: HIGH]
- Area: return-statement lifting / multret tracking (ast return handling / restructure).
- `return (two())` (truncates multret to 1) decompiled as `return two()` (returns all). In ARGUMENT position `print((two()))` the parens ARE kept — bug is specific to RETURN.
- Repro A `_harness/gen/pcall-error__pcall-error-pcall-truncation-paren.luau`: orig `true 1 nil`, decompiled `true 1 2`.
- Repro B `_harness/gen/pcall-error__pcall-error-select-hash-with-nil.luau`: `pick(...)=(select(3,...))` orig `30`, decompiled `30 40`. (all O)

### C6 — captured per-iteration local eliminated → closure rebinds to shared loop var  [severity: HIGH]
- Area: copy elimination / capture handling (copy_cleanup or inline + closure capture).
- `while ... do local x = i; fns[i] = function() return x end; i += 1 end` — decompiler drops `local x = i`
  and makes the closure capture the loop variable `i` directly. Each closure must capture a FRESH cell.
- Repro `_harness/gen/closures-upval__closures-upval-loopvar-while.luau`: orig `1 2 3`, decompiled `4 4 4`. (all O)

### C7 — `if c then return a() else return b() end` collapsed to and/or ternary truncates multret  [severity: HIGH]
- Area: conditional_expressions / if_expression / normalize_conditions (return-if collapse).
- `if flag then return a() end return b()` (a returns 1,2,3) decompiled as `return not p and 9 or a()`.
  The and/or expression truncates `a()` to ONE value.
- Repro `_harness/gen/multret-vararg__multret-vararg-conditional-multret.luau`: orig returns 1,2,3 (tcount 3),
  decompiled returns 1 (tcount 1). (O2)

## DYNAMIC TALLY: 31 true mismatches across 618 programs (O0/O1/O2) = 7 distinct root-cause bugs C1-C7
- C1 NaN-flip: 16 manifestations | C2 table-ctor: 4 | C3 loop-multiret-reorder: 1 | C4 upvalue-snapshot: 6
- C5 return-trunc: 2 | C6 loopvar-capture: 1 | C7 multret-ternary: 1
- C4 is the most pervasive; C2-C7 reproduce at ALL opt levels (core lifter/SSA, not readability passes).

### C2b — formatter drops non-positive / fractional numeric keys (saturating f64→usize cast)  [HIGH] (static #24, broadens C2)
- `ast/src/formatter.rs:1252`: `are_table_keys_sequential` tests `(x - 1f64) as usize == i`. `f64 as usize` saturates(neg→0)+truncates. So `[0]`,`[-1]`,`[0.5]`,`[1.5]`,`[2.5]` are judged "sequential" → key dropped → value relocated to array slot. Affects DIRECT literals too (not just rebuilt).
- Repro `_harness/_t_tkey.luau`: `t[0]="zero"` → `{"zero"}` so t[0]=nil,t[1]="zero"; `u[1.5]` → `{ "frac" }`; `{"x",[0]="y"}` → `{"x","y"}`. All wrong (values+#length). VERIFIED.

### C8 — `for ... do break end` (runtime bound) → whole function dropped  [HIGH, data-loss] (static #14)
- `restructure/src/loop.rs:92-93` OOB `then_successors[0]`. At O0, `for i=1,n do break end` → `-- failed to decompile` (entire function lost). VERIFIED O0 (`_harness/_t_fb2.luau`). O2 optimizes the loop away.

### C9 — SSA inliner reorders observable side effects  [HIGH] (static #10)
- `cfg/src/ssa/inline.rs:222-229`. `local c1=A(); local m=B(a); ... c1+m` — inlining c1 past `m=B(a)` swaps A/B call order. VERIFIED O0 (`_harness/_t_f10.luau`): side-effect log `A,B7` → `B7,A`.

### C10 — captured-local snapshot eliminated; upvalue read moved PAST a mutating call  [HIGH] (static #16)
- `ast/src/inline_temps.rs` / copy_cleanup: `local captured = source; bump(); return captured` (snapshot before mutation) → `bump(); return source`. Reads upvalue AFTER mutation. The inverse of C4.
- VERIFIED O0 AND O2 (`_harness/_t_f16.luau`): orig `1`, decompiled `99`.

### C11 — empty `if` drops a relational comparison that can raise a runtime error  [MEDIUM] (static #15)
- `restructure/src/jump.rs:33-46`. `if a < b then end` with plain-local operands → dropped entirely. If `<` errors (e.g. two tables), the error is lost.
- VERIFIED O0 (`_harness/_t_eif3.luau`): pcall orig `false` (errors), decompiled `true`. (Side-effecting operands ARE preserved — only effect-free-but-erroring comparisons are dropped.)

## VERIFIED-CODE lifter findings (real code; reachable mainly via hand-crafted/obfuscated bytecode — RELEVANT since this tool targets obfuscated Roblox bytecode)
- L1 (#1/#4): `lifter.rs:313-318` LOADB with C>1 pushes edge to index+2 (assumes C==1); for C>1 → wrong block or `block_to_node().unwrap()` panic. Stock compiler emits C==1 only. CODE VERIFIED.
- L2 (#2): `lifter.rs:808` LOADKX (in deserializer aux list, function.rs:64) has no lift arm → `unreachable!` panic. Triggers with a proto >32768 constants. CODE VERIFIED.
- L3 (#5): `lifter.rs:1348` CMPPROTO non-zero D jump edge discarded (lowered as fall-through). Hand-crafted v11 only.
- L4 (#6): `lifter.rs:1363` non-JUMPX E-form (e.g. COVERAGE) → `unreachable!` panic. Coverage builds only.
- L5 (#7): `instruction.rs:156` NATIVECALL decoded as AD not ABC. Hand-crafted only.
- L6 (#8): `lifter.rs:1414` STRING constant index 0 → `string_table[v-1]` underflow panic. Hand-crafted only.
- (#21): `set_list.rs:81-100` SetList::Display fallback truncates a multret tail to one slot — plausible, NO trigger constructed.

## SPECULATIVE / investigated-not-reproduced (report honestly, do NOT claim as bugs)
- #22 naming avoid_shadowing may collide with a read global (speculative, no repro).
- #23 simplify_gotos plan==2 may drop a for back-edge continue (speculative, no repro).
- #25 math.pi emitted where a local `math` shadows (could not reproduce; folding removes the conflict).
- #3 Vector constant → `Vector3.new` — correct under Roblox; not a bug in the real target environment.

### C12 — `break` dropped when reconstructing complex nested loops  [HIGH] (NEW, found in final wave)
- `restructure/src/loop.rs` break-target resolution. In ≥3-deep nests where an inner loop has MULTIPLE break
  targets and a middle loop also breaks, the middle `break` is omitted → extra iterations.
- Repro `_harness/gen2/nestedctrl__nestedctrl_triple_break_label.luau`: `break` after `break-outer-j @ 2,2`
  dropped → extra `break-j @ 2,3,1`, count 18 → 19. VERIFIED. (Simple 2-3 level single-break loops are fine.)

### C13 — assignment to a LIVE local dropped as `local _ = expr` [HIGH] (from user report; CONFIRMED in v9 corpus)
- SSA "unused result" / dead-store misclassification. Emits `local _ = expr`, losing the write to a still-live local.
- (a) closure-captured: local read ONLY via closure upvalue judged dead → `x = sig:Connect(...)` → `local _ = ...`;
  x stays nil → `if x then x:Disconnect() end` dead → connection leak. (b) self-update `x=x+1` / guarded default dropped.
- CORPUS PROOF (fresh binary): `corpus_fresh/Client/HangingPlacement.client.luau`: `local v22 = nil` (:45),
  `local _ = localPlayer.AncestryChanged:Connect(...)` (:1449, should be `v22 = ...`), `if v22 then v22:Disconnect()` (:1507)
  → v22 forever nil → cleanup dead. Facet (b) suggestive: GiftcodeAdminUI `if not tonumber(...) then local _ = #v2 + #v3 end`.
- NOT reproducible via stock luau-compile v11 (v9-real-bytecode SSA shape); residual of merged `fix/closure-captured-local`.
- Distinct from C4 (stale read) / C6 (wrong capture): here the WRITE is dropped. Not among the original 12.

## WAVE 2 FULL (245 programs incl. coroutines/strpack/tablib/nestedctrl/tablekey) — 45 mismatches, 1 NEW family (C12)
- coroutines: C3 (fib generator `a,b=b,a+b` → powers of 2), C4 (upvalue-after-yield-loop snapshot), C6, C7 all
  reproduce THROUGH coroutine bodies. ~12 loopcarry C3 confirmations. tablekey_index_then_60_flush = C2 at the
  SETLIST flush boundary (60 entries). Everything maps to C1-C12; only C12 is new.

## WAVE 2 (74 programs: buffer + loopcarry + multret-ctx; 7 categories pending re-run) — reinforces, no new families
- 18 true mismatches, ALL map to C1-C7. No new bug family => taxonomy is complete for compiler-emitted bytecode.
- C3 canonical minimal: `for _=1,10 do x,y = y, x+y end` (Fibonacci) → powers of two (`gen2/loopcarry__loopcarry_swap_advance`).
  Confirms C3 = loop-carried parallel-copy swap bug; very common pattern.
- C5 broadened: truncation `(…)` lost for `return (vararg)`, `return (unpack)`, `return (select)`, `return (string.byte)`,
  and via inline_temps `local x=(select(...)); return x` → `return select(...)`. ~8-10 shapes. C5 is pervasive.
- C6 can CRASH: per-iteration captured buffer offset → all closures read final offset → `buffer access out of bounds`.
- C2/C4/C7 each re-confirmed with new buffer-backed shapes.
- NOTE: 7 wave-2 agents (coro/strpack/tablib/nestedctrl/tablekey/mixed-adversarial/upval-stress) hit transient API
  errors; re-running via resume to cover coroutines etc.

## FALSE POSITIVES identified (NOT bugs — for harness hygiene)
- Error-message text differences: Luau runtime errors embed the source FILENAME and LINE NUMBER, which legitimately
  differ between `orig.luau` and `*.dec.luau`. Affected pcall/xpcall/assert tests print these via tostring(err).
  e.g. pcall-error-assert-custom-message, pcall-error-assert-nonstring-msg, pcall-error-xpcall-handler-args.
  -> triage must normalize file paths + line numbers in error strings before comparing.
- The whole `goto-labels` generated category: Luau has NO goto/`::label::`; luau-compile rejects the SOURCE,
  so they are COMPILE_ERR (skipped), not decompiler bugs.

