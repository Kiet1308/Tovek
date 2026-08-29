#!/usr/bin/env bash
# Shared tool discovery for the differential harness.
set -u

HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HARNESS_DIR/.." && pwd)"
WORKSPACE_ROOT="$(cd "$REPO_ROOT/.." && pwd)"

_tool_from_env_or_candidates() {
  local variable="$1"; shift
  local configured="${!variable:-}"
  if [ -n "$configured" ]; then
    printf '%s\n' "$configured"
    return
  fi
  local candidate
  for candidate in "$@"; do
    if [ -f "$candidate" ]; then
      printf '%s\n' "$candidate"
      return
    fi
  done
  printf '%s\n' "$1"
}

LC="${LUAU_COMPILE_EXE:-}"
[ -n "$LC" ] || LC="$(_tool_from_env_or_candidates LUAU_COMPILE_EXE \
  "$WORKSPACE_ROOT/luau-tools/luau-compile.exe" \
  "$REPO_ROOT/luau-tools/luau-compile.exe" \
  "$WORKSPACE_ROOT/luau-tools/luau-compile" \
  "$REPO_ROOT/luau-tools/luau-compile")"

LUAU="${LUAU_EXE:-}"
[ -n "$LUAU" ] || LUAU="$(_tool_from_env_or_candidates LUAU_EXE \
  "$WORKSPACE_ROOT/luau-tools/luau.exe" \
  "$REPO_ROOT/luau-tools/luau.exe" \
  "$WORKSPACE_ROOT/luau-tools/luau" \
  "$REPO_ROOT/luau-tools/luau")"

LIFT="${LIFTER_EXE:-}"
[ -n "$LIFT" ] || LIFT="$(_tool_from_env_or_candidates LIFTER_EXE \
  "$REPO_ROOT/target/release/luau-lifter.exe" \
  "$REPO_ROOT/target/debug/luau-lifter.exe" \
  "$REPO_ROOT/target/release/luau-lifter" \
  "$REPO_ROOT/target/debug/luau-lifter")"

_tool_available() {
  local tool="$1"
  if [[ "$tool" == */* || "$tool" == *\\* ]]; then
    [ -f "$tool" ]
  else
    command -v "$tool" >/dev/null 2>&1
  fi
}

# WSL can launch a Windows executable, but the executable does not understand
# Linux mount paths such as /mnt/d/... passed as input arguments. Convert only
# existing absolute path arguments when wslpath is available; flags and scalar
# arguments (for example -O2) remain byte-for-byte unchanged. Native Linux
# tools and non-WSL shells take the fast path without any conversion.
run_tool() {
  local tool="$1"
  shift
  if [[ "$tool" == *.exe ]] && command -v wslpath >/dev/null 2>&1; then
    local converted=() arg
    for arg in "$@"; do
      if [[ "$arg" == /* ]] && [ -e "$arg" ]; then
        converted+=("$(wslpath -w "$arg")")
      else
        converted+=("$arg")
      fi
    done
    "$tool" "${converted[@]}"
  else
    "$tool" "$@"
  fi
}

require_tools() {
  local missing=0 tool
  for tool in "$LC" "$LUAU" "$LIFT"; do
    if ! _tool_available "$tool"; then
      echo "HARNESS_ERROR: required executable not found: $tool" >&2
      missing=1
    fi
  done
  [ "$missing" -eq 0 ] || {
    echo "Set LUAU_COMPILE_EXE, LUAU_EXE, and LIFTER_EXE to override tool paths." >&2
    return 2
  }
}

# A fixture may opt into one expected failure mode with a sidecar containing one
# token: compile, orig_runtime, decompile, decompiled_runtime, or mismatch.
expected_failure_for() {
  local source="$1"
  local metadata="${source%.luau}.expect"
  if [ -f "$metadata" ]; then
    tr -d '[:space:]' < "$metadata"
  fi
}
