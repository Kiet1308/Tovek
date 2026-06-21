#!/usr/bin/env bash
# run1.sh FILE [OPT] -> shows orig vs decompiled output and diff
set -u
f="$1"; OPT="${2:-2}"
LC=/d/Medal/luau-tools/luau-compile.exe
LUAU=/d/Medal/luau-tools/luau.exe
LIFT=/d/Medal/medal-decompiler/target/release/luau-lifter.exe
name="$(basename "$f" .luau)"
bc="/tmp/$name.bc"; dec="/tmp/$name.dec.luau"
"$LC" --binary "-O$OPT" "$f" > "$bc" 2>/tmp/cerr || { echo "COMPILE_ERR"; cat /tmp/cerr; exit 2; }
orig="$("$LUAU" "$f" 2>&1)"; orc=$?
if [ $orc -ne 0 ]; then echo "ORIG_RUNERR:"; echo "$orig"; exit 3; fi
"$LIFT" "$bc" > "$dec" 2>/tmp/derr || { echo "DECOMP_FAIL"; cat /tmp/derr; exit 4; }
decout="$("$LUAU" "$dec" 2>&1)"; drc=$?
if [ $drc -ne 0 ]; then echo "RUNDEC_FAIL:"; echo "$decout"; echo "--- dec src ---"; cat "$dec"; exit 5; fi
if [ "$orig" == "$decout" ]; then echo "PASS  $name"; else
  echo "MISMATCH  $name"
  echo "--- expected ---"; echo "$orig"
  echo "--- got ---"; echo "$decout"
  echo "--- dec src ---"; cat "$dec"
fi
