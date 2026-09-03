# PR #3 — `ForInitSuffixOrder` root cause, why the reviewer contract was built on a false premise, and the fix

> **Repository:** `Kiet1308/Tovek` · **PR:** #3 · **Base of this note:** `4ed6945` (PR head), lifter `6ef3cf4` for the numbers quoted as "before".
> **Corpus:** `examplebytecode/RobloxProject` (3,978 inputs, bytecode v9, key 203), run with `--strict-no-synthetic-control`.

## Tóm tắt (Vietnamese)

- **Nguyên nhân gốc:** 229 function / 159 file bị reject `ForInitSuffixOrder` **không hề có instruction nào chạy sau `FORGPREP`/`FORNPREP`**. Cái "suffix" là *phi-copy của SSA* (edge transfer init→header) mà destructor đẩy xuống **sau** marker; và các hằng số/`{}`/`math.huge` trong đó là `local x = {}` đứng **trước** vòng `for` trong source, bị SSA inliner gộp vào edge argument. Reviewer coi IR sau SSA là "sự thật bytecode" ("suffix executes once after preparation") — sai tiền đề, nên toàn bộ contract `PrepCommutingOnce`/effect-summary/interprocedural proof là chứng minh một thứ không cần chứng minh.
- **Fix đúng tầng:** destructor đặt edge-copy **trước** marker (khôi phục đúng thứ tự bytecode), chỉ giữ lại sau marker phần nào đọc giá trị do marker định nghĩa (counter/control phi) hoặc chạm upvalue cell. Không cần đổi gì ở region.rs cho class này; check `ForInitSuffixOrder` vẫn còn làm lưới an toàn.
- **2 class còn lại:** alias `ipairs`/`pairs`/`next` là upvalue không bao giờ bị ghi (Lerps) → chấp nhận (đó chính là proof của compiler khi phát `FORGPREP_INEXT/NEXT`); loop-result bị closure capture *bên trong* thân loop, không dùng ở ngoài (GameUpgrade) → chính là source `for` thuần.
- **Kết quả:** strict mode corpus **3,936 ok / 0 fail / 0 marker** (trước: 160 fail); 3,978 output binary-compile bằng compiler Luau chính thức; round-trip semantic 30/30 (10 fixture × O0/O1/O2, chạy thật bằng `luau` và so stdout); workspace test xanh (102 test restructure, +3). Ngoài ra sửa lỗi nhân đôi phần đuôi chung của `if` trong builder của PR3 (shared-tail + guard pass): corpus chỉ còn +1,0% dòng so với `main` (mà `main` vẫn còn 3 file `controlFlowState`), số call-site de-inline khôi phục được tăng 540 → 559.

---

## 1. What the "suffix" actually is

The lifter always ends a basic block at `FORGPREP*`/`FORNPREP` (they are in the
terminator list in `lifter.rs`). Therefore **no original instruction can follow
the `GenericForInit`/`NumForInit` marker inside the init block**. Everything that
appears after the marker in the post-destruct IR comes from exactly one place:

```
cfg/src/ssa/destruct.rs :: lift_block_params
    ...
    self.function.block_mut(assign_block).unwrap().push(parallel_assign.into());
```

i.e. the lowered **edge transfer** (phi copies) of the init→header edge, pushed
at the *end* of the init block, which is after the marker.

Two kinds of values reach that transfer:

1. plain register copies `t = x` (phi resolution — no bytecode instruction at all);
2. definitions that `ssa::inline` folded into the edge argument. The inliner only
   pulls a definition from **the same block, before the marker** (`for stat_index
   in (0..block.len()).rev()`), and only if it is not observable
   (`ast::is_observable` = has side effects or not total-pure).

So a suffix `count = 0`, `seen = {}`, `best = math.huge`, `origin = Vector3.new(0,0,0)`
is the source's `local count = 0` … **executed before the loop**, moved after
the prep by the decompiler's own SSA pipeline. Emitting it before the source
`for` is not a commutation that needs an effect proof: it *restores* the
bytecode order.

### 1.1 Empirical census (whole corpus, `MEDAL_DUMP_CFG=1`)

229 rejected functions; every post-marker statement classified by shape:

| shape after marker | count |
|---|---:|
| numeric `local = {…}` | 204 |
| numeric `local = literal` (`0`, `""`, `nil`, `true`) | 127 |
| generic `local = {…}` | 42 |
| generic `local = literal` | 28 |
| numeric `local = local` (pure phi copy) | 12 |
| numeric `local = math.huge` | 12 |
| numeric `local = Vector3.new(0,0,0)` | 9 |
| numeric `local = function() end` | 4 |
| `local = a or 3` / `-math.huge` / `not x` | 3 |
| generic `local = Vector3.new(…)` | 1 |

Not one call with arguments, index write, `Close`, `SetList`, or global write.
**Every function with a generic-loop suffix also had a numeric-loop suffix**, and
`build_numeric_loop` rejected *any* non-trivia suffix unconditionally
(`region.rs`, "FORNPREP has no source-level slot…"). That single rule is what
failed 159 files, including trivial ones such as `Sift/Array/reverse.lua`
(`local reversed = {}` + `for i = #array, 1, -1`).

### 1.2 Reproduction without the private corpus

`docs/failure_fixtures/semantic_roundtrip/suffix_numeric.luau` compiled with the
official compiler (`luau-compile --binary -O2 --fflags=false`, bytecode v9)
produces, post-destruct:

```
-- NumForInit
local i, limit, step = 1, #list, 1
-- end NumForInit
best = math.huge
seen = {}
count = 0
bestItem = nil
```

and `6ef3cf4` rejects the whole file with `ForInitSuffixOrder`. The pre-inline
dump shows the same values on the init→header edge as
`args=[best_h <- math.huge, seen_h <- {}, count_h <- 0, i_h <- i, limit_h <- limit, step_h <- step, …]`.

## 2. Assessment of the reviewer's proof contract

| Reviewer item | Assessment |
|---|---|
| "A FORGPREP-edge suffix executes once after preparation and before the first FORGLOOP" | **False premise.** True of the *post-SSA IR*, not of the bytecode. The values were produced before the prep; the pipeline moved them. |
| Class A `RenameOnlyEdgeTransfer` | Correct, but incomplete: it only covers `t = x`. The dominant shape (`{}`, literals, `math.huge`) is an inliner-folded definition, which the contract routes to class B. |
| Class B `PrepCommutingOnce` + `EffectSummary` + `commute(prep, suffix)` + "interprocedural effect proof for `__iter`" | **Unnecessary.** Nothing is commuted; the original order is restored. The only real obligations are (i) the transfer must not read a value the marker defines, (ii) it must not clobber a marker input, (iii) it must not move an upvalue-cell access across a prep that may run user code. Those are 3 local checks, not an effect calculus. |
| `StatementOrigin` provenance through SSA destruction | Not needed once the destructor places the copy correctly; provenance is implied by construction (post-marker statements can only be edge transfers). |
| "Never sequentialize a parallel-copy cycle" | Already handled by `sequentialize` (spill temp); `parallel_copy_cycle.luau` (Fibonacci / 3-rotation) round-trips at O0/O1/O2. |
| `ForOriginPrepKindUnsupported` value-flow lattice with immutable capture-by-reference proof | Over-scoped. The compiler emits `FORGPREP_INEXT/NEXT` only after proving the callee is a never-written alias chain to the builtin; the specialized opcode *is* that proof. Printing the alias call is the exact source form and recompiles to the same opcode. A within-function check ("incoming upvalue, never written by any statement or edge") is sufficient and cheap. |
| `CapturedLoopResultRef` may-open analysis over `CLOSEUPVALS` | Cannot be built where the reviewer put it: `Close` statements are consumed and dropped during SSA construction (`construct.rs`, `retain(!Close)`), so nothing post-destruct can see them. More importantly it is not needed for the decision: a result captured *inside its own loop body* and never touched outside the loop is, by definition, the source `for` body capture; the compiler closes captured loop-scope locals on every fallthrough/`continue`/`break`. The decompiler's own risk is only renaming/exporting/hoisting the result, which is what must be checked. |
| Strict mode "not necessarily zero typed rejections" | Rejected as a goal: with the correct placement there is no compiler-generated shape left in this corpus that needs a rejection. Zero failed files **and** strict mode are both satisfied. |
| Pin the Luau commit in CI | Agreed and done (`c2ec0d4`, release 0.736). |
| Fixture list (suffix/order, origin, captured result, fallback) | Covered by `docs/failure_fixtures/semantic_roundtrip/` (10 sources × O0/O1/O2, executed and compared) plus the existing unit tests. |

## 3. The fix (3 targeted changes, +560/−55 lines including tests/docs)

### 3.1 `cfg/src/ssa/destruct.rs` — place the init-edge transfer before the prep marker

`split_edge_transfer_around_for_prep`: when the predecessor block ends in
`GenericForInit`/`NumForInit`, the parallel copy is split into

- **before the marker:** every `(dst, src)` whose `src` reads no local written by the
  marker and no upvalue-group local, and whose `dst` is not a marker input / cell;
- **after the marker (old behaviour):** the rest — typically the loop-carried
  counter/limit/step or control phis, which coalesce away anyway.

Splitting is sound because every destination is a fresh temporary that no element
reads. Liveness, interference and coalescing run *after* this placement, so a
destination can never be merged with a marker input it would clobber.

Effect: the "suffix" disappears at the source. Both the source-like builder and
the legacy matcher see `[pre…, transfers…, MARKER]`. The existing
`ForInitSuffixOrder` / `CapturedCellReorder` / `ForProtocolEdgeTransfer` checks
in `region.rs` are untouched and remain a fail-closed net for the residual
"reads a marker output / touches a cell" transfers.

### 3.2 `restructure/src/region.rs` — specialized prep kinds through a stable upvalue alias

`validate_for_origins` now computes `stable_upvalues` = protected locals that are
not parameters and are written by no statement and no edge transfer in the
function. `source_proves_for_prep_kind_with_alias_and_upvalues` accepts such a
callee for `pairs(…)`/`ipairs(…)` calls and for the `next, state[, nil]` tuple
forms (the same-block "latest write is the builtin global" rule is kept and now
also applies to the tuple form). Everything else still rejects.

### 3.3 `restructure/src/region.rs` — loop-owned ref-captured result

`captured_result_is_loop_owned`: a ref-captured generic-for result is accepted
iff it is not renamed (`rewrite`), not exported, and no statement or edge outside
the loop's own nodes reads, writes or captures it. The three existing negative
tests (post-loop edge write, direct-exhaustion write, parameter alias) still
reject.

### 3.4 Readability: shared tails, guards, retry

PR #3's builder rebuilt a conditional's arms with a reset `visited` set, so a
tail reachable from both arms (or from an arm and the enclosing region's stop)
was emitted once per arm; a 262-line file became 454 lines and the corpus grew
by ~8k lines versus `main`.  `shared_tail_join` now finds the earliest node
both arms flow into whose pre-join node sets are disjoint (or uses the
enclosing walk's stop when the other arm terminates), builds the arms up to
it, and emits the tail once; every attempt is validated (the join must still
be unvisited) and rolled back, and a whole-function retry without tail sharing
guarantees no function loses its structured output.  Redundant trailing
`continue`s are stripped at loop-body ends, and a late `flatten_guards` pass
turns `if c then <body> end; return x` into the idiomatic guard form after the
de-inline pass has matched its copies.

## 4. Verification

| Check | Before (`6ef3cf4`) | After |
|---|---:|---:|
| Corpus, strict mode: decompiled / failed | 3,775 / **161** | **3,936 / 0** |
| Function diagnostics `ForInitSuffixOrder` / `PrepKindUnsupported` / `CapturedLoopResultRef` | 229 / 2 / 1 | 0 / 0 / 0 |
| Outputs containing `controlFlowState`, `goto`, `GenericFor*`, `NumForInit`, `__close_uv` | 0 | 0 |
| Outputs that binary-compile with official `luau-compile -O0` | 3,817 / 3,817 | **3,978 / 3,978** |
| Corpus size vs pre-PR `main` output (which still had 3 `controlFlowState` dispatchers) | +12,227 lines at PR head equivalent | **+5,352 lines** (+1.0%), 0 dispatchers |
| De-inline call-site recoveries (`-- inlined by Luau -O2`) vs `main` | 552 | **559** (main: 540) |
| `docs/failure_fixtures/residual_control_flow` (default + strict) | 5 ok / 2 FAIL | 7 ok / 0 FAIL |
| `scripts/semantic_roundtrip.py` (10 sources × O0/O1/O2 → decompile strict → recompile → **execute & compare stdout**) | 13 / 24 (11 no output) | **30 / 30** |
| `cargo test --workspace --all-targets` | green | green (restructure 102, +3 new tests; 2 tests re-targeted to the new contract) |

The round-trip fixtures cover the reviewer's requested classes: init-edge copy
elimination, interference-free parallel copies, a parallel-copy cycle
(Fibonacci/rotation), zero-trip loops with staged initializers, an observable
`__iter` metamethod (trace order preserved), a captured upvalue cell read by a
custom iterator, ref-captured results with body mutation / `break` / `continue` /
`return function() … end`, module-level `ipairs`/`pairs`/`next` aliases (INEXT /
NEXT at O1/O2), nested loops with exported inner results and the Pet-shaped
`found`/`break` pattern.

Also verified: the Pet bytecode from the original report (`bug/Pet.lua.bytecode.b64`)
and `FishTrainingPopupArea` still decompile with no `controlFlowState`.

## 5. What is deliberately *not* claimed

- Hand-crafted bytecode that emits `FORGPREP_INEXT` with a non-`ipairs` callee, or
  captures a loop result without any `CLOSEUPVALS`, is not compiler output and
  has no Luau source form; the emitted `for … in callee(t)` / body capture is the
  only faithful source representation and is what the legacy structurer always
  emitted. Modelling `Close` as a typed loop-boundary event (reviewer §6) would
  require keeping `Close` provenance through SSA construction; it is a possible
  future hardening, not a blocker.
- The remaining small differences between the 161 newly emitted files and the
  pre-PR (`main`) output (e.g. a hoisted `local result, v2` in `Sift/Array/insert`)
  are PR #3's own structuring/declaration changes, not introduced here.

## 6. Diagnostics added

`MEDAL_DUMP_CFG=1` (with `--threads 1`) prints every function's blocks, statements
and edge arguments at `pre-inline`, `pre-destruct` and `post-destruct`, which is
how the census in §1.1 was produced; `MEDAL_DEBUG_RESTRUCTURE=1` still prints the
typed rejection per function.
