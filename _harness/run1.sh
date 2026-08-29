#!/usr/bin/env bash
# run1.sh FILE [OPT] -> shows orig vs decompiled output and diff
set -u
f="$1"; OPT="${2:-2}"
. "$(cd "$(dirname "$0")" && pwd)/common.sh"
require_tools || exit $?
name="$(basename "$f" .luau)"
WORK="$HARNESS_DIR/_run1_work"; mkdir -p "$WORK"
bc="$WORK/$name.bc"; dec="$WORK/$name.dec.luau"
run_tool "$LC" --binary "-O$OPT" "$f" > "$bc" 2>"$WORK/cerr" || { echo "COMPILE_ERR"; cat "$WORK/cerr"; exit 2; }
orig="$(run_tool "$LUAU" "$f" 2>&1)"; orc=$?
if [ $orc -ne 0 ]; then echo "ORIG_RUNERR:"; echo "$orig"; exit 3; fi
run_tool "$LIFT" "$bc" > "$dec" 2>"$WORK/derr" || { echo "DECOMP_FAIL"; cat "$WORK/derr"; exit 4; }
decout="$(run_tool "$LUAU" "$dec" 2>&1)"; drc=$?
if [ $drc -ne 0 ]; then echo "RUNDEC_FAIL:"; echo "$decout"; echo "--- dec src ---"; cat "$dec"; exit 5; fi
if [ "$orig" == "$decout" ]; then echo "PASS  $name"; else
  echo "MISMATCH  $name"
  echo "--- expected ---"; echo "$orig"
  echo "--- got ---"; echo "$decout"
  echo "--- dec src ---"; cat "$dec"
fi
