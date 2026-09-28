# v2.2: presentation and fewer temporaries

ROADMAP_V3 §3 asked v2.2 to close the generated-program structure gap without
changing the default statement convention: consistent compound assignment,
source-like function declarations, an opt-in `--style=compact`, and structure
reported for both styles. The V2.1 release notes also promised fewer
temporaries, if-expressions where they read better, and the friendly spellings
back where they are provably safe. This records what was built, what was
measured, and what was refused.

## Changes

| Change | Kind | Effect |
|---|---|---|
| Fold stores into zero-valued DUPTABLE template slots; never list a literal key twice | fix (V2.1 regression) | `{ Component = 0, ..., Component = component }` is gone; ~10k corpus lines |
| Inline effects past Luau's late register reads | temporaries | `local v4 = fn(...); v2 += v4` becomes `v2 += fn(...)` |
| Bare forward declarations | declarations | `local f = nil` becomes `local f` when `f` only ever receives closures |
| Compound assignment for globals | compound | `counter += 1` on a global, like locals and pure-keyed fields |
| Register-aware out-of-SSA coalescing | temporaries | a register's phi web is merged before copies between registers |
| Empty then-arm inversion | control flow | `if c then else B end` becomes `if not c then B end` |
| `--style compact` | opt-in style | scalar selects as if-expressions; value-exact boolean idioms |
| `--assume-standard-libraries` | opt-in spelling | `math.pi`, `math.huge`, `Vector3.new(...)` for folded constants |
| Literal concatenation/arithmetic are total | temporaries | `"slot" .. 1` moves like a literal |
| Style-normalized structure metric | benchmark | raw and style-fit fidelity reported separately |

### Late register reads

The Luau compiler hands a register local straight to arithmetic and comparison
instructions and to GETTABLE. In `v + f()`, `v < f()` and `v[f()]` the call runs
first and `v` is read when the operation executes (`CALL`, then `ADD Rv Rv Rc`),
at every optimization level. The inliner modelled the read as happening first,
so a captured `v` kept every call result in a temporary. A direct register
operand is no longer a barrier for an effect moved into the other operand.
Incoming upvalues (GETUPVAL) and concatenation operands (copied first) keep it.
Fixture `roadmap_v2/late_register_reads` covers all shapes at O0-O2.

An inlined closure is a compiler caveat worth knowing: at O2 the Luau inliner
turns `nested()`'s upvalue read into a late register read (`ADD R12 R1 R13`),
so the O2 bytecode itself differs from the source's O1 behaviour. Every
decompiler must reproduce the bytecode, so the fixture returns the closure
instead of calling it.

### Register-aware coalescing

Out-of-SSA copy coalescing was greedy in dominator order, so a copy between two
registers (`lastRescan = now`) could be merged first and drag the whole
`lastRescan` web into `now`:

```luau
-- before                                  -- after
if now - v12 >= 10 then                    if now - v12 >= 10 then
    rebuild(data)                              rebuild(data)
else                                           v12 = now
    now = v12                              end
end                                        for i = 1, #v11 do
for i = 1, #v11 do                             ...
    v12 = now                              end
    ...
    now = v12
end
v12 = now
```

The destructor now receives the lifter's SSA-version -> register map, gives each
phi transport its phi's register, and first merges only copies within one
register. Every merge is still interference-checked; only the choice of
surviving copies changes. Reassigned parameters also keep their identity
(`x = minValue ... x = maxValue` instead of assigning the clamp bound to the
other parameter). Fixture: `luau-lifter/tests/register_phi_webs.rs`.

### Compact style

`--style compact` (option bit `COMPACT_STYLE`) keeps v9 selects as
if-expressions and writes each remaining scalar select, where every arm is one
assignment of one value to the same local, as `x = if c then a elseif d then b
else e`. A missing arm is allowed only after the local's `nil` declaration.
Conditions and the chosen arm evaluate in the same order, and the assignment
truncates to one value exactly like each arm did. A select with a value-exact
boolean idiom uses it (`c and t`, `not c or t`) unless the other operand is
`nil`. Function and table literals and expressions longer than one line stay
statements.

### Library spellings stay opt-in

At O2 the compiler folds `math.pi`, `math.huge` and (with Roblox's vector
options) `Vector3.new(1, 2, 3)` into constants, but only while the script never
writes the global and never names getfenv/setfenv. `--assume-standard-libraries`
restores the spelling under exactly those conditions, so it compiles back to
the same constant. It stays opt-in because the spelling reads the environment
at run time, which another script can replace; the deep-review cases
`vector_environment` and `vector_nested_environment` check that the exact
default does not.

## Measurements

Frozen V2.1 benchmark plan (`release-v2.1-bench/expert/run`, v9, same pinned
compiler, VM and comparator). "raw" is the official AST SequenceMatcher ratio;
"style" additionally normalizes `x op= e`, single-target if-expressions and atom
parentheses on both sides; "LCS" is the exact longest common subsequence of the
same token stream. difflib's greedy blocks are not monotonic: in
`completed_snapshots`, printing the source's own bare `local helper` lowers the
raw ratio by 0.13 while LCS rises, so both are reported.

| Suite | Build | raw | style | LCS | Runtime |
|---|---|---:|---:|---:|---:|
| generated | V2.1 | 0.597 | 0.734 | 0.723 | 192/192 |
| generated | v2.2 default | 0.636 | 0.777 | 0.741 | 192/192 |
| generated | v2.2 compact | 0.677 | 0.793 | 0.770 | 192/192 |
| generated | lua.expert | 0.746 | 0.751 | 0.805 | 160/192 |
| public | V2.1 | 0.861 | 0.860 | 0.883 | compile 513/513 |
| public | v2.2 default | 0.866 | 0.864 | 0.887 | compile 513/513 |
| public | v2.2 compact | 0.867 | 0.867 | 0.888 | compile 513/513 |
| regression | V2.1 | 0.801 | 0.809 | 0.847 | 369/369 |
| regression | v2.2 default | 0.799 | 0.807 | 0.847 | 369/369 |
| regression | v2.2 compact | 0.794 | 0.801 | 0.849 | 369/369 |

Paired cluster bootstrap against lua.expert on generated programs:

| Build | raw | style | LCS |
|---|---|---|---|
| V2.1 | −0.149 [−0.172; −0.128] | — | −0.082 [−0.094; −0.068] |
| v2.2 default | −0.110 [−0.128; −0.091] | +0.026 [+0.011; +0.040] | −0.064 [−0.075; −0.052] |
| v2.2 compact | −0.069 [−0.084; −0.054] | **+0.043 [+0.027; +0.058]** | −0.035 [−0.045; −0.025] |

The compact acceptance criterion holds on the style-fit axis, where the interval
is positive. On the raw axis it does not: rewriting V2.1's compound assignments
as `x = x + e` alone gains +0.098 on generated programs, because that grammar
never writes `+=`. Real code does (874 of 1,009 update statements in the game
source; 145 of 223 in the pinned public libraries), and both spellings compile
to identical bytecode, so the default and compact styles keep `+=`. The
remaining raw gap is mostly that choice plus O2 helper arguments (below).

Default style against V2.1: generated +0.039 raw, public +0.005 raw, regression
−0.002 raw with LCS +0.000 (the difflib case above). Runtime, semantic
round-trip (45/45), deep review (199/199), v9/v12/v14 suites with determinism,
compiler witnesses (42/42) and both bytecode oracles pass; the oracles improve
from 29 to 27 (residual) and 16 to 13 (semantic) non-equivalent prototypes.
`--style compact` passes the v9 and v12 suites with determinism (258/258 each).

Private corpus (3,978 files, default style): 1,428 files change,
519,818 -> 509,162 lines. Compact style: 504,844 lines. Every output compiles
with the pinned compiler (six files with non-ASCII paths cannot be opened by the
CLI itself).

### Performance

Same machine, both built from source with the pinned toolchain and the release
profile (fat LTO). The machine was under memory pressure during these runs, so
single-thread wall times varied by up to 25% between identical runs; CPU time is
reported for that case.

| Workload | V2.1.1 | v2.2 |
|---|---:|---:|
| Corpus, 16 threads, median of 9 | 1.439 s | 1.362 s |
| Corpus, 1 thread, CPU median of 5 | 14.91 s | 14.77 s |
| Largest 12 scripts, best of 5 each | 873 ms | 845 ms |
| Peak working set, 16 threads | 97 MB | 98 MB |

New passes are single traversals, or fold into existing ones: the empty-then
inversion runs inside guard flattening, the duplicate-key check is a lazily
built hash set, and the extra coalescing sweep skips assignments without a
local copy before allocating.

## Refused

- **Library spellings by default.** Not provable without assuming no other
  script replaces the environment (see above).
- **`function f()` for forward-declared closures.** Real code writes
  `f = function` for 46 of 54 forward declarations; the formatter keeps it.
- **Moving an effect past a store into a fresh, unescaped table.** Sound, and it
  removes O2 helper-argument temporaries, but it changed constructor folding
  elsewhere (`local v4 = { Ability = object:KeyOf(...) }` instead of inlining the
  table into its call): regression suite −0.0045 raw for +0.0004 on generated.
- **A three-sweep coalescing order** (program copies before cross-register phi
  transports): no output change on 3,978 files.
- **Compound assignment through an impure base** (`local v = t[i]; v.X += e` ->
  `t[i].X += e`, 24 corpus sites): exact, but the AST has no compound node; the
  formatter would print a double evaluation. Deferred.

## Remaining

The generated O2 configuration is the weakest (compact style at O2 without
debug info: raw −0.125 against lua.expert):
the compiler inlines `tap(label, item)` and the argument temporaries stay
because the effectful argument must not cross the inlined body's table store.
That needs the fresh-table escape proof above without disturbing constructor
folding.
