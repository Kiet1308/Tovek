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
LC=/d/Medal/luau-tools/luau-compile.exe
LUAU=/d/Medal/luau-tools/luau.exe
LIFT=/d/Medal/medal-decompiler/target/release/luau-lifter.exe
WORK="$DIR/_work_O$OPT"
mkdir -p "$WORK"

pass=0; mismatch=0; decfail=0; runfail=0; origfail=0
: > "$DIR/_results_O$OPT.txt"
report() { echo "$1" | tee -a "$DIR/_results_O$OPT.txt"; }

for f in "$DIR"/*.luau; do
  [ -e "$f" ] || continue
  name="$(basename "$f" .luau)"
  bc="$WORK/$name.bc"
  dec="$WORK/$name.dec.luau"
  # 1. compile
  if ! "$LC" --binary "-O$OPT" "$f" > "$bc" 2> "$WORK/$name.cerr"; then
    report "COMPILE_ERR  $name : $(head -1 "$WORK/$name.cerr")"
    continue
  fi
  # 2. run original
  orig="$("$LUAU" "$f" 2>&1)"; orc=$?
  if [ $orc -ne 0 ]; then
    report "ORIG_RUNERR  $name (orig program itself errors; skip): $(echo "$orig" | head -1)"
    origfail=$((origfail+1)); continue
  fi
  # 3. decompile
  if ! "$LIFT" "$bc" > "$dec" 2> "$WORK/$name.derr"; then
    report "DECOMP_FAIL  $name : $(head -1 "$WORK/$name.derr")"
    decfail=$((decfail+1)); continue
  fi
  # 4. run decompiled
  decout="$("$LUAU" "$dec" 2>&1)"; drc=$?
  if [ $drc -ne 0 ]; then
    report "RUNDEC_FAIL  $name (decompiled output won't run): $(echo "$decout" | head -2 | tr '\n' ' ')"
    runfail=$((runfail+1)); continue
  fi
  # 5. compare
  if [ "$orig" == "$decout" ]; then
    pass=$((pass+1))
  else
    report "MISMATCH     $name"
    report "  --- expected (orig) ---"; echo "$orig" | head -20 | sed 's/^/  /' | tee -a "$DIR/_results_O$OPT.txt"
    report "  --- got (decompiled) ---"; echo "$decout" | head -20 | sed 's/^/  /' | tee -a "$DIR/_results_O$OPT.txt"
    mismatch=$((mismatch+1))
  fi
done
echo "============================================================"
echo "O$OPT  PASS=$pass  MISMATCH=$mismatch  DECOMP_FAIL=$decfail  RUNDEC_FAIL=$runfail  ORIG_RUNERR=$origfail"
echo "============================================================"
