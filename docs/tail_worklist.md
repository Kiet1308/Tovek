# Common-tail factoring: revisit the changed interval

Common-tail factoring used to walk every child tree again after each committed action. The new traversal keeps one contiguous interval of statements to revisit: the changed `if` plus any tail statements inserted immediately after it. It still searches the entire parent block for the next action. Candidate preference, equality, capture identity, declaration protection, ownership preparation and all three rewrite rules remain unchanged.

## Why the interval is sufficient

The initial bottom-up walk brings every child to the same fixed point as before. The existing actions have these mutation boundaries:

| Action | Mutated children | New parent statements |
|---|---|---|
| `ReuseParent` | One or both arms of the selected `if` | None |
| `MergeArms` | Both arms of the selected `if` | One arm's common suffix, immediately after the `if` |
| `HoistLeafTails` | Selected leaves in the then arm; emptied else arm | Former else tail, after the final `if` |

Each action leaves other parent statements' child trees and tail contexts unchanged. The changed `if` must be revisited: moving its tail can remove its previous return/continue context, and truncating arms can expose more work inside them. The inserted statements are also revisited. The **whole parent search remains necessary** because insertion can create a new following continuation or an opportunity in an earlier `if`. The optimization does not advance a cursor past possible new parent candidates.

Entry-point block unsharing remains mandatory. Mutating an arm must not alter an unrelated sibling through a shared block container. Closure function handles and captures keep their existing identities. The matcher compares closure identity/prototype and captures without inspecting callback bodies; whole-chunk traversal visits closure roots with the same return context. Function-only factoring continues to avoid child function bodies that may belong to another worker.

The interval uses constant additional space and retains no AST/local owner or cached semantic summary. There is no new rewrite, approximate equality, skipped proof, helper synthesis or scheduling-dependent budget. Future action variants must preserve or explicitly extend this mutation boundary.

## Differential and corpus validation

The original full child-rescan algorithm is retained as a test-only specialization. At every recursive block it uses the previous schedule, so comparison is not limited to the top-level loop. A fixed seed generates 1,200 structured trees, each checked at three tail contexts and three declaration-protection settings: **10,800 comparisons**, including refusals, overlapping tails, loops, scoped locals and comments. These are structural IR schedule tests, not new VM-equivalence claims. Independent closure graphs test shared callback ownership; a focused fixture checks inserted-tail adjacency and earlier parent candidates. The existing factoring tests still cover repeat/continue exclusions, branch scope and trailing annotations.

The full Rust workspace/all-targets run passes 983 primary tests plus one child repeat. The Python suite passes 112 tests, including counter-corruption controls. All 180 runtime profiles, 513 public profiles and 42 compiler-witness profiles preserve complete source hashes, dataflow/fidelity results, observations and existing call-reconstruction events. Timing and temporary output paths are the only excluded row fields. All 42 compiled compiler-witness mutants are still detected. The 45 legacy semantic profiles and 52 size checks pass.

Detailed runtime/public artifacts pass parser, capture, emission, default/compact-comment, one/four-thread and cold/warm artifact-cache checks. Exact sidecar-byte comparison additionally preserves every prior field in 4,629 nonempty runtime/public/private artifacts. All 3,978 private source files, including 42 empty inputs, agree at 1/16 threads. `scripts/artifact_identity.py` checks actual source/sidecar file bytes and executable hashes. Only top-level command, executable identity, requested thread count and the invocation's current-directory Git HEAD may differ; thread count is checked against the command. Git HEAD is collected at export time and is not an original-bytecode or sidecar provenance fact.

## Counters and cost policy

The optional profiler adds four counters within `TAIL_SCAN` and `TAIL_SCAN_FUNCTION`:

- `tail_initial_child_visits`: immediate statements visited in initial block walks.
- `tail_actions`: committed factoring actions.
- `tail_revisited_children`: immediate statements revisited after actions.
- `tail_skipped_children`: unaffected immediate statements excluded from those post-action walks.

These count traversal occurrences, not unique nodes, allocation bytes or saved nanoseconds. A skipped immediate child can contain a large subtree. Parent candidate scans are still performed and are not counted as skipped work. Counter projections, existing node census and output must agree across 1/16 threads; timing fields are excluded only from that deterministic comparison. Diagnostic timings include profiling/export overhead and do not establish a release speedup.

Before release evaluation, the cost policy locked seven interleaved warm-filesystem CLI rounds over the complete private corpus at 1/16 threads. Median, nearest-rank p95 and median peak RSS may regress by at most 5% at each thread count. The goal is less redundant traversal, measured with counters; a wall-time speedup is claimed only when supported beyond noise. With seven rounds, nearest-rank p95 equals the maximum. Builds, tests and diagnostic profiles run separately from the benchmark.

On the 3,936 nonempty private scripts, each thread count produces 539,837 complete profile rows with the same counter/census projection hash. Whole-chunk factoring performs 539 actions, revisits 1,121 immediate children and skips 1,604; function-only factoring performs 38 actions, revisits 93 and skips 102. In total, 1,706 of the 2,920 immediate post-action child visits are excluded (58.42%). This is **not** a 58% reduction of all traversal or runtime: the initial block walks still count 746,104 immediate visits, and parent matching continues.

| Threads | Baseline median (s) | Worklist median (s) | Median change | p95 change | Median peak RSS change |
|---|---:|---:|---:|---:|---:|
| 1 | 24.687991 | 24.722491 | +0.14% | +0.08% | -2.11% |
| 16 | 1.889761 | 1.877833 | -0.63% | -3.72% | +0.43% |

All frozen cost limits pass. There is no established wall-time speedup. Median peak RSS is 34,672,640 -> 33,939,456 bytes at one thread and 118,947,840 -> 119,455,744 at 16 threads; maximum RSS changes -1.95%/+1.31%. Every measured source tree remains `755ee3c15f31f7174da3551eb301bae12ec8ed7597ab6abd5a2ea5ab014ac706`. The accepted change reduces repeated child work with bounded additional bookkeeping, while retaining the measured costs and limitations. [Acceptance inventory, full samples and lossless profiles](roadmap_v2_acceptance/tail_worklist_validation.json).

Reproduction uses the same native CLI and harnesses as the existing roadmap:

```text
cargo +nightly-2024-12-15 test --workspace --all-targets
cargo +nightly-2024-12-15 build --release --locked -p luau-lifter --bin luau-lifter
python scripts/artifact_identity.py --before BASELINE_ARTIFACTS --after CURRENT_ARTIFACTS --report identity.json
python scripts/profile_v2.py --before BASELINE --after CURRENT --corpus INPUT --key 203 --threads 1 16 --keep LOCAL_PROFILES --report profiles.json
python scripts/benchmark_v2.py --lifter before=BASELINE --lifter after=CURRENT --corpus INPUT --key 203 --threads 1 16 --rounds 7 --keep LOCAL_BENCHMARK --report benchmark.json
```

This implements a local worklist in common-tail factoring. Immutable-epoch summary caches, other passes' traversal strategies, complete pass/allocation census and OS-cold-cache measurement remain separate R7 work. R9 remains paused and AI remains disabled by default.
