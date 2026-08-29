#!/usr/bin/env bash
# Differential decompiler test harness.
# For each *.luau test program in DIR:
#   source --(luau-compile -O$OPT)--> bytecode --(luau-lifter)--> decompiled
#   run original + decompiled with luau.exe, compare stdout.
# A mismatch / decompile failure / run failure of the decompiled output is a bug.
#
# Usage: ./diff.sh DIR [OPT]   (OPT defaults to 2)
set -u
DIR="${1:?need dir}"
OPT="${2:-2}"
. "$(cd "$(dirname "$0")" && pwd)/common.sh"
require_tools || exit $?
WORK="$DIR/_work_O$OPT"
mkdir -p "$WORK"

pass=0; mismatch=0; decfail=0; runfail=0; origfail=0; compilefail=0
seen=0
: > "$DIR/_results_O$OPT.txt"
report() { echo "$1" | tee -a "$DIR/_results_O$OPT.txt"; }

for f in "$DIR"/*.luau; do
  [ -e "$f" ] || continue
  case "$f" in *.dec.luau) continue;; esac
  seen=$((seen+1))
  name="$(basename "$f" .luau)"
  expected="$(expected_failure_for "$f")"
  bc="$WORK/$name.bc"
  dec="$WORK/$name.dec.luau"
  # 1. compile
  if ! run_tool "$LC" --binary "-O$OPT" "$f" > "$bc" 2> "$WORK/$name.cerr"; then
    if [ "$expected" = "compile" ]; then
      report "EXPECTED_FAIL $name : compile"
      pass=$((pass+1)); continue
    fi
    report "COMPILE_ERR  $name : $(head -1 "$WORK/$name.cerr")"
    compilefail=$((compilefail+1)); continue
  fi
  # 2. run original
  orig="$(run_tool "$LUAU" "$f" 2>&1)"; orc=$?
  if [ $orc -ne 0 ]; then
    if [ "$expected" = "orig_runtime" ]; then
      report "EXPECTED_FAIL $name : original runtime"
      pass=$((pass+1)); continue
    fi
    report "ORIG_RUNERR  $name (orig program itself errors; skip): $(echo "$orig" | head -1)"
    origfail=$((origfail+1)); continue
  fi
  # 3. decompile
  if ! run_tool "$LIFT" "$bc" > "$dec" 2> "$WORK/$name.derr"; then
    if [ "$expected" = "decompile" ]; then
      report "EXPECTED_FAIL $name : decompile"
      pass=$((pass+1)); continue
    fi
    report "DECOMP_FAIL  $name : $(head -1 "$WORK/$name.derr")"
    decfail=$((decfail+1)); continue
  fi
  # 4. run decompiled
  decout="$(run_tool "$LUAU" "$dec" 2>&1)"; drc=$?
  if [ $drc -ne 0 ]; then
    if [ "$expected" = "decompiled_runtime" ]; then
      report "EXPECTED_FAIL $name : decompiled runtime"
      pass=$((pass+1)); continue
    fi
    report "RUNDEC_FAIL  $name (decompiled output won't run): $(echo "$decout" | head -2 | tr '\n' ' ')"
    runfail=$((runfail+1)); continue
  fi
  # 5. compare
  if [ "$orig" == "$decout" ]; then
    pass=$((pass+1))
  else
    if [ "$expected" = "mismatch" ]; then
      report "EXPECTED_FAIL $name : mismatch"
      pass=$((pass+1)); continue
    fi
    report "MISMATCH     $name"
    report "  --- expected (orig) ---"; echo "$orig" | head -20 | sed 's/^/  /' | tee -a "$DIR/_results_O$OPT.txt"
    report "  --- got (decompiled) ---"; echo "$decout" | head -20 | sed 's/^/  /' | tee -a "$DIR/_results_O$OPT.txt"
    mismatch=$((mismatch+1))
  fi
done
if [ "$seen" -eq 0 ]; then
  report "HARNESS_ERROR no .luau fixtures found in $DIR"
  exit 2
fi
echo "============================================================"
echo "O$OPT  PASS=$pass  COMPILE_ERR=$compilefail  MISMATCH=$mismatch  DECOMP_FAIL=$decfail  RUNDEC_FAIL=$runfail  ORIG_RUNERR=$origfail"
echo "============================================================"
[ $((mismatch + decfail + runfail + origfail + compilefail)) -eq 0 ]
