# Tovek V2.7 roadmap and handoff

State on 2026-10-28. Work happens on branch `v2.7-dev`, which starts at V2.6.1 (`77edf38`, where `main` still points).
Merging into `main` and releasing need the owner's go-ahead.

**Decompiler goal complete within the owner-approved scope, 2026-10-28.**
[Acceptance report](ACCEPTANCE.md): G1 and the three review cases without
specifications/reproducers were explicitly deferred by the owner. They remain
technically unresolved in the follow-up backlog. Release/site work is excluded.

## Done on v2.7-dev

| commit | change | measured |
|---|---|---|
| `aa3f767` | Rebuild helpers whose result register SSA reused before the last branch (`alias_merged_result`, `deinline/local_queries.rs`, return-window cache, SSA facts reuse) | fixture `deinline_merged_result_branches` |
| `c864e16` | Guards without a needless `else`, `repeat ... until`, one-shot loops removed (`ast/src/cleanup_control_flow.rs`) | dumps vs V2.6.1: 598 files, -982 non-blank lines, +14 rebuilt calls; oracle non-equivalent protos 297→289 (semantic), 29→22 (residual) |
| `125d12a` | Capture census built only where a pass can use it (`CaptureSafety` levels, `CaptureSafety::captures`) | byte-identical on 9,987 files; S_COND_EXPRS 162→77 ms, S_SYNTH_HELPERS 162→82 ms |
| `0da3755` | Roblox vector constants print as `Vector3.new(...)` (S3, `VectorSpelling`) | `createVector` header 616 files→0; `Vector3.new(` 1,486→4,068; -645 lines |
| `1aeabd8` | Live self-updates print as one variable (S2: `offset += 1`, `options = options or {}`) | 106 files, +18 lines, calls unchanged |

At `1aeabd8`, all checks pass:
- 1,636 workspace tests;
- 699/699 semantic round-trips;
- local CI 35/35.

## Current implementation checkpoint

Latest: [verified Ponytail review fixes](checkpoint_review_fixes.md), code
`4880984`, corrects generic vector constructor proof, numeric style checks,
readable conditional width after final naming and PromptTriggered roles.
The late readable pass also preserves captured writes and accounts for
expression indentation/shared AST bodies. It passes 1,704 workspace tests,
213 Python tests, all 33 audit steps, 807 semantic, 499 deep-review and 513
public cases. All old semantic/residual outputs and 510/513 public outputs
remain byte-identical; three public profiles retain captured writes, adding
21 lines without losing rebuilt calls. Baselines only gain new fixtures.
No fresh four-version benchmark or 135,000-profile stress claim is made for
this binary; the owner-approved G1/private-input deferrals remain unchanged.

The preceding [singleton phi inputs without per-phi sets](checkpoint_phi_singleton.md)
keeps the raw-alias, captured-cell and malformed-schema rules while reducing
allocation. It passes 1,698 workspace tests, all 33 audit steps, 801 semantic,
469 deep-review and 513 public cases. All semantic/public/residual outputs
remain byte-identical. Two longer A/B runs favor four workers; single-worker
speedup remains uncertain. On 2026-10-28 the owner accepted the measurements
and deferred remaining G1 work; eliminating the regression no longer gates
this goal. The report retains the contrary short run and all samples.

The preceding [split prefix stores before open SETLIST](checkpoint_split_prefix.md)
restores private repeated/separated prefix stores while retaining native bulk
growth, key-error timing and allocation guards. It passes 1,697 workspace tests,
801 semantic and 469 deep-review cases, all 33 audit steps, 1,476 table probes,
108 VM comparisons and 288 targeted fuzz profiles. Every old semantic/public/
residual output remains byte-identical. The preceding [vector producer
checkpoint](checkpoint_vector_producer.md) retains and validates emitted vector
bindings. [Broad stress on that split-prefix binary](stress_split_prefix.md) is complete:
133,906 passed / 1,094 invalid / zero failed or refused across 135,000 profiles;
mutation has 7,695 passed / 904 typed refusals / 401 invalid / zero failed.
Both 512-case structuring probe sets also passed on the earlier `b798e5b`
binary, with byte-identical outputs against the split-prefix checkpoint.
The [three review cases lacking specifications/reproducers](remaining_review_cases.md)
remain unresolved and were separately deferred by the owner; broad coverage
does not identify the original missing inputs.

The checkpoint paragraphs below retain the status at each checkpoint; the
current scope/status section and acceptance report define the final scope.

## Historical checkpoints

See [control flow, naming and proof validation](checkpoint_control_naming.md) for
the next checkpoint after `76212c9`: while-header recovery, bounded readable
if-expressions, naming tiers/event roles, malformed-bytecode refusals and the
exact vector-fold prerequisite. The full decompiler goal remains in progress;
[GOAL.md](GOAL.md) tracks remaining scope, including M6. The owner confirmed that
the private dumps are unavailable and selected the existing structural line
census policy (no new above-floor gate).

The follow-up [vector argument checkpoint](checkpoint_vector_arguments.md) now
restores both reflect calls using the shared exact fold proof. It passes 1,655
workspace tests and the full audit, with byte-identical public outputs against
`927885c`. Other remaining work below is still open.

The [copy-capture checkpoint](checkpoint_copy_capture.md) fixes a pre-existing
fuzz miscompile when SSA reuses a snapshotted local. Its full audit passes and
public outputs remain byte-identical; the report records two conservative
semantic-fixture call refusals. The complete goal remains open.

The [M6 L1/L2 checkpoint](checkpoint_final_ast.md) adds source-informed spacing
and final-AST return proofs. Its full audit passes; per-file rebuilt-call
comments are unchanged. The report records intentional line-count changes and
the measured public memory/runtime cost. L4 and the remaining goal stay open.

The [runtime-nil table checkpoint](checkpoint_table_nil.md) preserves NEWTABLE
collision placement when record values can become nil at runtime. Its full
audit passes; public and existing semantic line counts and per-file call
comments are unchanged. Capacity mismatches and the rest of the goal remain open.

The [naming/placeholder checkpoint](checkpoint_naming_placeholder.md) removes
`__index`-only class hints, adds irregular collection names, proves private
placeholder stores and accepts two larger allocation cases. All 33 audit
steps pass; old per-file line/call counts are unchanged. The [implementation
audit](audit_existing_m4_m6.md) records existing S1 gates and the shared L4
engine/cache.

The [naming consensus checkpoint](checkpoint_naming_consensus.md) requires
compatible RHS roles before publishing assignment hints and adds literal
object-name evidence. Incoming parameter values and unknown writes veto narrow
RHS names. All 33 audit steps pass, with 1,687 workspace tests, 786 semantic
cases and 445 deep-review cases; all old per-file line/call counts are unchanged.
N1–N3 have implementation and available validation. L3, performance, final
stress checks and the explicitly missing review material remain open.

The [formatter checkpoint](checkpoint_performance_public.md) caches one adjacent
source-line census per block and preserves every byte across 786 semantic, 513
public and 7 residual outputs. All 1,688 workspace tests and 33 audit steps pass.
The [L3 evaluation](evaluation_eq_polarity.md) keeps current EQ polarity: the
global switch increased semantic lines and nesting. G1 remains open.

The [mixed table growth checkpoint](checkpoint_mixed_table_growth.md) fixes a
valid-bytecode refusal found by broad fuzz: explicit array keys still count as
hash syntax in mixed constructors. All 1,689 workspace tests, 33 audit steps,
1,476 table probes and 512 structuring probes pass; 459 reruns of failing seed
profiles pass. Existing semantic/public/residual outputs remain byte-identical.
Broad fuzz across the full range and final performance acceptance continue.

## Binding decisions

These come from the owner. Do not reopen them.

- **D1: never assume the standard library is untouched.** Scripts can hook or replace `math`, `type` and the rest.
  - No builtin folding or evaluation to justify a match.
  - No tier-1 spelling bundle, not even behind a flag.
- **D2: every rebuilt call keeps its comment.** Wording may get shorter, but no site loses its comment.
- **D3: readability first for if-expressions.** Use an if-expression where it reads clearest: two short arms in a value position. Otherwise keep statements.
- **D4: keep handle preservation.** `GetService` and `require` handles stay as named locals. Fold only duplicate `GetService` paths and constructor/template slots.
- **Empty hash allocations (owner decision, 2026-10-27):** preserve legacy empty-constructor output instead of refusing public inputs. Exact `pairs`/`next` order across different recompilation optimization levels remains unproven here; populated constructors and fold/match proofs keep their layout checks.
- **G1 (owner decision, 2026-10-28):** accept the current measurements and move remaining performance work to a follow-up. The V2.5.1 regression remains real/unresolved; removing it is no longer an acceptance condition for this V2.7 decompiler goal. Preserve all benchmark evidence.
- **Missing review cases (separate owner decision, 2026-10-28):** defer case (c), the original repeat/return O2 case and the three original generic-for O2/g2 seeds until specifications/reproducers are available. Complete the validated decompiler scope now; do not label these deferred cases fixed.
- **`error(msg, level)`:** a different line position in the message never blocks a helper rebuild.
- **Naming:**
  - Evidence names come first.
  - A type-derived fallback (`p` → `str`) is allowed below every evidence rule, with per-scope suffixes.
  - Never use file-wide counters (`str64`), shadowing names or `__index`-derived class names.
  - Never use AI or ML naming, not even as an option.
- **Performance work must keep output byte-identical.** Rejected ideas go to `docs/performance/dead_ends.md`. Read that file before proposing engine work.
- **Fixtures:** every edge case found becomes a fixture in `docs/failure_fixtures/semantic_roundtrip`. Add it with its oracle and output-size baselines in the same commit.
- **Git:**
  - Stage explicit paths only.
  - Never stage `.lua` files (the test corpus), `docs/tess.c` or `luac.out`.
  - `*.md` and `*.json` are gitignored; force-add (`git add -f`) only files meant to be published.
  - The repository is public: private corpora appear only as aggregate numbers.

## Checks before each commit

1. **Workspace tests:** `cargo +nightly-2026-06-15 test --workspace --all-targets`.
   - Use `-j 4` on a machine with 16 GB of RAM. Many jobs at once ran out of paging space (os error 1455).
2. **CI audit job:** everything in the "Fixture and Luau audit" job of `.github/workflows/ci.yaml`.
   - On the original machine, `v22-work/tools/ci_local.sh <lifter.exe> <outdir>` runs all 35 suites.
   - `lineage` must run before `binding_graph` and `profile`, which read its `lineage.json`.
3. **Output diff on real dumps (for any output change):**
   - Decompile the four frozen dumps (9,987 inputs) with the binary before and after the change, and read the diff.
   - Total lines and rebuilt calls must not regress.
   - On the original machine: `v22-work/tools/dumps.sh <exe> <label>` writes `v22-work/tmp/dumps-<label>`.
   - Kept dumps there: `v261` = V2.6.1, `s3c` = `0da3755`, `s2c` = `1aeabd8`, `frdyn251` = V2.5.1.
4. **Baselines:** every output change regenerates the oracle baseline (`docs/bytecode_roundtrip/baseline_semantic_roundtrip.json`) and the output-size baseline (`docs/output_size/baseline_fixtures.json`).

## Decompiler scope and current status

### 1. Performance — remaining work deferred by the owner

On 2026-10-28 the owner selected “Chấp nhận số đo hiện tại, chuyển G1 sang việc
tiếp theo”. G1 no longer blocks completion of this goal. The regression has
not been fixed; the figures and investigation below remain the follow-up baseline.

**Measurement.**
- Instrumented single-thread corpus run (`MEDAL_PROFILE_JSON=<file>`, per-pass spans): 15.56 s at V2.6 plus the V2.7 work, against 14.01 s for V2.5.1.
- The owner's uninstrumented release run on 2026-10-12, V2.5.1 against the build of that day:
  - one thread: 11.57 s against 13.06 s;
  - 16 threads: 0.97 s against 1.15 s.

| phase | change vs V2.5.1 | note |
|---|---|---|
| D_SCAN | +382 ms | de-inline fixpoint rounds went from 1,359 to 2,580 (probe/admit/assign); every productive round rebuilds the whole `CaptureSafety` census (span `D_ROUND_CENSUS`) |
| DECOMPILE (exclusive) | +185 ms | |
| DESER_LIFT | +117 ms | |
| DESER_INLINED_COPIES | +111 ms | new in V2.6 (line-evidence census) |
| S_FORMAT | +101 ms | |
| naming | about +100 ms | |
| D_COLLECT_TARGETS | +87 ms | |
| LITERAL_SHARING_CENSUS | +83 ms | new, already limited to chunks with targets |
| S_EXPR_DEINLINE / S_DEINLINE | +50 / +42 ms | |
| S_COND_EXPRS, S_SYNTH_HELPERS | fixed in `125d12a` | |

**Current cloud measurement.** [Checkpoint benchmark](benchmark_phi_singleton.md)
uses 25 interleaved ordinary-release samples per version on 513 public inputs.
At `b798e5b`, median 1/4-worker time is 15.5%/15.4% above V2.5.1 and
3.3%/4.2% above V2.6.1; paired ratios and substantial noise are included in
the report. Cross-run changes are not estimates of a code change's effect.
[CPU call stacks](checkpoint_cpu_profile.md) provide separate diagnostics.
[Additional process CPU time](checkpoint_cpu_time.md) does not establish a
single-worker gain from singleton phi; the empty-cell-map candidate was rejected.
The original performance target remains unmet, with further work deferred by
the owner. The older owner measurements above are a separate workload and machine.

**Follow-up backlog (outside the current goal after the owner decision).**
- Avoid rebuilding the census each D_SCAN round: keep it per function and invalidate only what a round changed, or skip rounds that cannot admit anything.
- Then D_COLLECT_TARGETS, S_FORMAT and the two DESER phases.

**Earlier attempt.** The owner's own pass on 2026-10-12 is in `docs/performance/2026-10-12-deinline.md`, kept locally. Its local-census and window-cache changes are in `aa3f767`. It gave no end-to-end speedup.

**Profiling.** Without admin rights, WPR and xperf do not work. Use the line-table build (`--profile fastrel`) and sample with `cdb -pv -p <pid> -c "~* kc 60; qd"`.

### 2. M4 structure — validated; undefined handle-split case deferred

S1/S4 and while-header recovery have implementation and available validation;
see [the implementation audit](audit_existing_m4_m6.md),
[control-flow checkpoint](checkpoint_control_naming.md) and
[placeholder proof](checkpoint_naming_placeholder.md). The list below records
the requirements, not a new implementation backlog. Only “case (c)” lacks a
definition in the accessible material and was deferred by the owner on
2026-10-28. L3 was evaluated and the current EQ polarity retained;
[measured reasons](evaluation_eq_polarity.md).

**S1: small exact structure gates.**
- `guard-continue` gate and tail-duplication limit.
- Terminal if/else ordered by weight.
- `factor_common_tails` scope check.
- `negate_exact` plus guard split; it never flips a relational operator (NaN).
- Truthiness collapse.
- and/or chains, with truthiness only from literals and constructors (D1).
- Comparison orientation in the SSA inliner.
- Snapshot-noise refinement and colon precision.
- Index tuple and swap folds.
- Statement residue, refused when the `if` sits in a loop that does not contain the declaration.
- Handle splits without case (c): deferred pending its definition, by owner decision.

**S4: readable if-expressions.**
- Two atomic arms, at most 72 columns, value positions only.
- A placeholder prints `nil` instead of the loader's `0` only when nothing can read the table before that key's first store.

**`while` with an inlined condition.** Case: `open_cases/while_inlined_condition.luau`.
- Previously emitted `while true do if not isAlive(p) then break end ... end`.
- Now recovers the loop header from the rebuilt call, preserving its comment
  and evaluation/continue behavior; pinned and semantic fixtures cover it.

**L3: EQ-jump polarity and branch order.** Evaluated; retain current polarity
because the measured global switch increased semantic lines and nesting.

### 3. M5 naming (N1 to N3) — implemented and validated

[Consensus checkpoint](checkpoint_naming_consensus.md),
[naming/placeholder checkpoint](checkpoint_naming_placeholder.md) and
[control-flow/naming checkpoint](checkpoint_control_naming.md) cover the
requirements below, including both formerly wrong names.

- **N1:** a `NameTier` on each local, an override matrix, consumer evidence, and vetoes for misleading names.
- **N2:** version-tagged Roblox API tables (names only, never annotations), literal evidence (`.Name = "X"`), module/class/self roles (keep `recover_methods`' B3 rule), and restoration of discarded bindings when the frame has headroom.
- **N3:**
  - unanimous phi votes;
  - tuple roles and irregular plurals;
  - the typed fallback (`p` → `str`), reported as a weak name.
- **Resolved naming cases:**
  - `open_cases/event_param_named_from_parent_store.luau`: event evidence now keeps `character` despite the `.Parent = x` store; a pinned assertion and semantic fixture cover it.
  - Digit names `vector2`/`vector3`: naming and S3 reservation are now implemented and tested.

### 4. Leftovers from the V2.6 review

1. **`repeat` with a `return` inside an `if`, at -O2 — original case deferred pending reproducer.** The old review reported a refusal in both versions, with an inlined return jumping past the `until` exit and a proposed small terminal-block copy. Public repeat/inlining probes pass on the current binary; without the original input this is not a confirmed fix of that review case.
2. **Nested inlined calls in a generic `for` at -O2 -g2 — original seeds deferred pending data.** The old review reported refusals in 3 of 45,000 seeds and suggested SSA debug-local boundaries or destruct edge-copy placement. Current public probes pass; the three original seeds remain unidentified. The owner deferred both entries on 2026-10-28; see [conditions for resuming](remaining_review_cases.md).
3. **Known malformed-bytecode families now refuse before lifting**, with unit and deep-review controls. Original private bytes behind generic panic labels remain unavailable:
   - `unreachable LOP_BREAK`;
   - `unwrap` on `None`;
   - "not implemented";
   - a FORGPREP without its partner.
4. **Table layouts: bounded allocation/fold/match model implemented.**
   - [Allocation checkpoint](checkpoint_table_layout.md): 1,679 workspace tests, 771 semantic cases, 415 deep-review cases, 1,476 table probes and all 33 audit steps. Preserve both hash/array hints; shared layout proofs guard folding and helper matching. Empty hash constructors retain the owner-selected compatibility policy; output growth, one conservative semantic call refusal and encoding limits are documented.
   - Runtime-nil NEWTABLE record values are handled by [the formatter proof](checkpoint_table_nil.md); 18/513 public outputs change key spelling with no line/call regression. The older estimate of 478 private dump files cannot be checked here.
5. **Vector constant arguments are implemented.** The shared exact fold model restores both `reflect` calls; [proof and validation](checkpoint_vector_arguments.md). Library constructor lookups are not evaluated to prove a match.
6. **Line-census policy is settled:** the owner selected the existing policy. Structural rebuilds above the size floor keep their current proof; no new mandatory line-census gate.

### 5. M6 — included in full scope; implemented and validated

[L1/L2 checkpoint](checkpoint_final_ast.md) and [L4 audit](audit_existing_m4_m6.md)
cover these requirements. Remaining performance work under G1 was deferred
by the owner's 2026-10-28 decision.

- L1: blank lines follow the source lines (`SourceLines` kept out of `PartialEq`).
- L2: return types on the final AST, proven types only.
- L4: consolidate the de-inline engine (expression mode replaces the legacy family, incremental helper cache).

### 6. Release V2.7 — explicitly outside this task

The owner excluded release, version changes, website/changelog work and merging
main. The following is a future release checklist, not work authorized here.
Engine benchmark evidence is recorded above; remaining G1 optimizations are
a separate follow-up under the owner's decision.

1. Bump the version: crate versions and the CLI string `luau-lifter V2.7`.
2. Run the checks above, then a release benchmark against V2.6.1 and V2.5.1. Claim no speedup without clean measurements.
3. Build a changelog page `docs/changelog/v2.7/` in its own new design.
   - Each release so far had a different look.
   - No copy about the page's own design.
   - Code shown on the site comes only from public sources or our own samples.
4. Rebuild the in-browser module (`v22-work/v26/wasm/build.sh <checkout>`) and refresh the site data (`v22-work/v26/data/make_all.py`).
5. Tag the release; the `release.yaml` workflow builds the 4 platforms. Write release notes in the V2.6 style.
6. Fast-forward `main` to the release (Pages deploys from `main`).

## Gotchas found during V2.7

- **S3 vector spelling.** Printing `vector.create(...)` in place failed the deep-review cases `vector_environment` and `vector_nested_environment`:
  - Code outside the chunk can replace a function's environment after load (`getfenv(f).vector = fake`).
  - For chunks that are not Roblox client bytecode, the per-chunk snapshot is the only exact spelling.
- **S2 variants rejected.**
  - Without the per-function gate: 3 rebuilt calls lost on a tween library.
  - Gated only on "inlined by the compiler": one large script lost 55 calls and gained 382 lines.
  - Not covered: `value = math.clamp(value, ...)`. Copy propagation removes the MOVE that proves the update, so the pass stays strict.
- **Line endings.** `luau-lifter/src/lib.rs` uses LF; some other files use CRLF. Edit those as bytes: a text-mode rewrite turns the whole file into a diff.
- **Non-ASCII paths.** `luau-compile` cannot open them; copy the file to an ASCII path first.
- **Low memory on the original machine.** It has 16 GB of RAM, usually 1 to 3 GB free. A process that cannot start under low memory exits with 0xC0000142: that is not a test failure. Never run CI, profiling and builds at the same time.

## Local material on the original machine (not in git)

- **Owner's 2026-10-12 work logs:**
  - `D:/Medal/_tmp/v27-implementation`;
  - `D:/Medal/_tmp/v27-performance-20261012`;
  - `D:/Medal/_tmp/special-20261013`.
- **Owner's plan notes:**
  - `docs/PLAN_V2_7_PERFORMANCE.md`;
  - `docs/PLAN_V2_7_READABILITY.md`;
  - `docs/performance/2026-10-12-deinline.md`.
- **Full M0 to M6 design with gains and dependencies:** `D:/Medal/v22-work/tmp/deinline-research/results/v2-plan.md`.
- **V2.6 final-review reports:** `fr-*.md` in the same folder.
- **Tools:** `D:/Medal/v22-work/tools/` (`ci_local.sh`, `dumps.sh`, `phases.py`, `baseline_add.py`).
- **Binaries:**
  - variants: `D:/Medal/v22-work/variants/*.exe` (`s2c.exe` = `1aeabd8`);
  - Luau tools: `D:/Medal/luau-tools-src/build`.
