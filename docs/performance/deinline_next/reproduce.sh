#!/usr/bin/env bash
# Reproduce de-inline timing evidence from the repository root on Linux.
# Usage: bash reproduce.sh WORK BASELINE NEXT REFERENCE LUAU_COMPILE LUAU_RUNNER
set -euo pipefail
if [[ "$#" != 6 ]]; then
  echo 'Usage: reproduce.sh WORK BASELINE NEXT REFERENCE LUAU_COMPILE LUAU_RUNNER' >&2
  exit 2
fi
work=$(realpath -m "$1")
baseline=$(realpath "$2")
next=$(realpath "$3")
reference=$(realpath "$4")
compiler=$(realpath "$5")
runner=$(realpath "$6")
if [[ -e "$work" ]]; then
  echo 'Use a new work directory so corpora cannot contain stale files.' >&2
  exit 2
fi
mkdir -p "$work/tmp"
export TMPDIR="$work/tmp"
# These are OS affinity IDs, not a claim about physical cores or CPU quota.
one_cpu=${DEINLINE_ONE_CPU:-2}
many_cpus=${DEINLINE_MANY_CPUS:-2,3,4,5}
rounds=${DEINLINE_ROUNDS:-15}
python3 scripts/deinline_workloads.py --compiler "$compiler" --output "$work/synthetic" --replicas 16
python3 scripts/semantic_roundtrip.py --compiler "$compiler" --luau "$runner" --lifter "$next" --keep "$work/semantic"
python3 - "$work" "$baseline" "$next" "$reference" <<'PY'
import pathlib,shlex,shutil,sys
root=pathlib.Path(sys.argv[1])
small=root/'small';small.mkdir()
for source in sorted((root/'synthetic/input').glob('small_scope_*.lua')):
    shutil.copyfile(source,small/source.name)
assert len(list(small.glob('*.lua')))==16
for count in (8,16):
    out=root/f'aggregate-{count}';out.mkdir()
    for label,binary in zip(('baseline','next','reference'),sys.argv[2:]):
        script='#!/usr/bin/env bash\nset -euo pipefail\nfor ((invocation=0; invocation<'+str(count)+'; invocation++)); do\n  '+shlex.quote(binary)+' "$@"\ndone\n'
        path=out/label;path.write_text(script);path.chmod(0o755)
PY
for cohort in synthetic fixtures; do
  if [[ "$cohort" == synthetic ]]; then corpus="$work/synthetic/input"; else corpus="$work/semantic/in"; fi
  for threads in 1 4; do
    if [[ "$threads" == 1 ]]; then affinity="$one_cpu"; else affinity="$many_cpus"; fi
    taskset -c "$affinity" python3 scripts/benchmark_v2.py \
      --lifter "baseline=$baseline" --lifter "next=$next" --lifter "reference=$reference" \
      --corpus "$corpus" --key 1 --threads "$threads" --rounds "$rounds" \
      --keep "$work/bench-$cohort-t$threads" --report "$work/bench-$cohort-t$threads.json"
  done
done
taskset -c "$one_cpu" python3 scripts/benchmark_v2.py \
  --lifter "baseline=$work/aggregate-16/baseline" --lifter "next=$work/aggregate-16/next" --lifter "reference=$work/aggregate-16/reference" \
  --corpus "$work/small" --key 1 --threads 1 --rounds "$rounds" \
  --keep "$work/bench-small-t1-aggregate16" --report "$work/bench-small-t1-aggregate16.json"
taskset -c "$many_cpus" python3 scripts/benchmark_v2.py \
  --lifter "baseline=$work/aggregate-8/baseline" --lifter "next=$work/aggregate-8/next" --lifter "reference=$work/aggregate-8/reference" \
  --corpus "$work/semantic/in" --key 1 --threads 4 --rounds "$rounds" \
  --keep "$work/bench-fixtures-t4-aggregate8" --report "$work/bench-fixtures-t4-aggregate8.json"
printf 'Completed; reports and generated corpora are in %s\n' "$work"
