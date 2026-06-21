#!/usr/bin/env bash
# Per-bug regression checker.
# For each _bugs/C*.luau: compile -O{0,1,2}, decompile with the WORKTREE binary,
# run orig + decompiled, compare normalized stdout. PASS = outputs match at all
# opt levels (or the level is skipped because the source itself errors / no diff
# applies). Prints a one-line verdict per bug.
#
# Usage: ./check.sh [bug ...]   (default: all C*.luau in _bugs/)
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
LC=/d/Medal/luau-tools/luau-compile.exe
LUAU=/d/Medal/luau-tools/luau.exe
LIFT="${LIFTER_EXE:-/d/Medal/medal-wt-review/target/release/luau-lifter.exe}"
WORK="$HERE/_check_work"; mkdir -p "$WORK"

norm() { sed -E 's#[^ ]*\.luau:[0-9]+(:[0-9]+)?#FILE:N#g; s#[^ ]*\.luau#FILE#g'; }

bugs=("$@")
if [ ${#bugs[@]} -eq 0 ]; then
  bugs=()
  for f in "$HERE"/_bugs/C*.luau; do
    [ -e "$f" ] || continue
    case "$f" in *.dec.luau) continue;; esac
    bugs+=("$(basename "$f" .luau)")
  done
fi

allpass=1
for name in "${bugs[@]}"; do
  src="$HERE/_bugs/$name.luau"
  [ -e "$src" ] || { echo "?? $name : no source"; continue; }
  orig="$("$LUAU" "$src" 2>&1)"; orc=$?
  if [ $orc -ne 0 ]; then echo "ORIGERR $name (source itself errors; skip)"; continue; fi
  verdict="PASS"; detail=""
  for opt in 0 1 2; do
    bc="$WORK/$name.O$opt.bc"; dec="$WORK/$name.O$opt.dec.luau"
    if ! "$LC" --binary "-O$opt" "$src" > "$bc" 2>"$WORK/$name.cerr"; then
      continue   # source not compilable at this level (rare); skip
    fi
    if ! "$LIFT" "$bc" > "$dec" 2>"$WORK/$name.derr"; then
      verdict="DECOMP_FAIL"; detail="O$opt: $(head -1 "$WORK/$name.derr")"; break
    fi
    decout="$("$LUAU" "$dec" 2>&1)"; drc=$?
    if [ $drc -ne 0 ]; then
      verdict="RUNDEC_FAIL"; detail="O$opt: $(echo "$decout" | head -1)"; break
    fi
    if [ "$(echo "$orig" | norm)" != "$(echo "$decout" | norm)" ]; then
      verdict="MISMATCH"; detail="O$opt: orig[$(echo "$orig" | tr '\n' '|')] != dec[$(echo "$decout" | tr '\n' '|')]"; break
    fi
  done
  if [ "$verdict" = "PASS" ]; then
    echo "PASS    $name"
  else
    echo "FAIL    $name : $verdict $detail"
    allpass=0
  fi
done
[ $allpass -eq 1 ] && echo "=== ALL PASS ===" || echo "=== SOME FAIL ==="
