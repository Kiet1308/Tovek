#!/usr/bin/env bash
# Per-bug regression checker.
# For each _bugs/C*.luau: compile -O{0,1,2}, decompile with the WORKTREE binary,
# run orig + decompiled, compare normalized stdout. Any unexpected tool, compile,
# or runtime failure is a failed verdict; fixtures may opt into one expected
# failure using a `<fixture>.expect` sidecar.
#
# Usage: ./check.sh [bug ...]   (default: all C*.luau in _bugs/)
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/common.sh"
require_tools || exit $?
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
if [ ${#bugs[@]} -eq 0 ]; then
  echo "HARNESS_ERROR: no .luau fixtures found in $HERE/_bugs" >&2
  exit 2
fi
for name in "${bugs[@]}"; do
  src="$HERE/_bugs/$name.luau"
  [ -e "$src" ] || { echo "FAIL    $name : no source"; allpass=0; continue; }
  expected="$(expected_failure_for "$src")"
  orig="$(run_tool "$LUAU" "$src" 2>&1)"; orc=$?
  if [ $orc -ne 0 ]; then
    if [ "$expected" = "orig_runtime" ] || [ "$expected" = "compile" ]; then
      echo "PASS    $name (expected source failure)"
    else
      echo "FAIL    $name : ORIGERR $(echo "$orig" | sed -n '1p')"
      allpass=0
    fi
    continue
  fi
  verdict="PASS"; detail=""
  for opt in 0 1 2; do
    bc="$WORK/$name.O$opt.bc"; dec="$WORK/$name.O$opt.dec.luau"
    if ! run_tool "$LC" --binary "-O$opt" "$src" > "$bc" 2>"$WORK/$name.cerr"; then
      if [ "$expected" = "compile" ]; then
        verdict="EXPECTED_FAIL"; detail="O$opt: compile failure"; break
      fi
      verdict="COMPILE_FAIL"; detail="O$opt: $(sed -n '1p' "$WORK/$name.cerr")"; break
    fi
    if ! run_tool "$LIFT" "$bc" > "$dec" 2>"$WORK/$name.derr"; then
      if [ "$expected" = "decompile" ]; then
        verdict="EXPECTED_FAIL"; detail="O$opt: decompile failure"; break
      fi
      verdict="DECOMP_FAIL"; detail="O$opt: $(head -1 "$WORK/$name.derr")"; break
    fi
    decout="$(run_tool "$LUAU" "$dec" 2>&1)"; drc=$?
    if [ $drc -ne 0 ]; then
      if [ "$expected" = "decompiled_runtime" ]; then
        verdict="EXPECTED_FAIL"; detail="O$opt: decompiled runtime failure"; break
      fi
      verdict="RUNDEC_FAIL"; detail="O$opt: $(echo "$decout" | sed -n '1p')"; break
    fi
    if [ "$(echo "$orig" | norm)" != "$(echo "$decout" | norm)" ]; then
      if [ "$expected" = "mismatch" ]; then
        verdict="EXPECTED_FAIL"; detail="O$opt: expected mismatch"; break
      fi
      verdict="MISMATCH"; detail="O$opt: orig[$(echo "$orig" | tr '\n' '|')] != dec[$(echo "$decout" | tr '\n' '|')]"; break
    fi
  done
  if [ "$verdict" = "PASS" ] || [ "$verdict" = "EXPECTED_FAIL" ]; then
    echo "PASS    $name"
  else
    echo "FAIL    $name : $verdict $detail"
    allpass=0
  fi
done
if [ "$allpass" -eq 1 ]; then
  echo "=== ALL PASS ==="
  exit 0
fi
echo "=== SOME FAIL ==="
exit 1
