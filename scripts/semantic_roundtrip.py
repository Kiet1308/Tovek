#!/usr/bin/env python3
"""Semantic round-trip check for the generic/numeric `for` structuring proofs.

For every `*.luau` under the fixture directory:

1. compile it with the official Luau compiler at -O0, -O1 and -O2 (bytecode
   version 9, the version Roblox ships, via `--fflags=false`);
2. decompile the bytecode with the lifter in strict (fail-closed) mode;
3. binary-compile the emitted source with the official compiler;
4. execute the original and the emitted source under the official `luau`
   CLI and compare stdout and the exit status.

Any missing output, recompile failure, or stdout/exit mismatch fails the run.

Usage:
    semantic_roundtrip.py --compiler PATH --luau PATH --lifter PATH [--fixtures DIR]
"""
import argparse
import base64
import pathlib
import shutil
import subprocess
import sys
import tempfile


def run(cmd, **kw):
    return subprocess.run(
        cmd, capture_output=True, text=True, encoding="utf-8", errors="replace", **kw
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--compiler", required=True, help="luau-compile executable")
    parser.add_argument("--luau", required=True, help="luau (REPL/runner) executable")
    parser.add_argument("--lifter", required=True, help="luau-lifter executable")
    parser.add_argument(
        "--fixtures",
        default=str(pathlib.Path(__file__).resolve().parent.parent
                    / "docs" / "failure_fixtures" / "semantic_roundtrip"),
    )
    parser.add_argument("--keep", help="directory to keep the work tree in")
    args = parser.parse_args()
    # Resolve tool paths up front: a relative executable path is not looked up
    # reliably by subprocess on every platform.
    for name in ("compiler", "luau", "lifter"):
        setattr(args, name, str(pathlib.Path(getattr(args, name)).resolve()))

    fixtures = pathlib.Path(args.fixtures)
    work = pathlib.Path(args.keep) if args.keep else pathlib.Path(tempfile.mkdtemp(prefix="tovek_rt_"))
    if work.exists():
        shutil.rmtree(work)
    inp = work / "in"
    inp.mkdir(parents=True)

    cases = []
    for src in sorted(fixtures.glob("*.luau")):
        for opt in ("0", "1", "2"):
            name = f"{src.stem}__O{opt}"
            raw = subprocess.run(
                [args.compiler, "--binary", f"-O{opt}", "-g1", "--fflags=false", str(src)],
                capture_output=True,
            )
            if raw.returncode != 0 or not raw.stdout:
                print(f"[COMPILE FAIL] {name}: {raw.stderr.decode(errors='replace')[:300]}")
                return 1
            if raw.stdout[0] != 9:
                print(f"[UNEXPECTED BYTECODE VERSION] {name}: {raw.stdout[0]}")
                return 1
            (inp / f"{name}.lua").write_text(base64.b64encode(raw.stdout).decode())
            cases.append((name, src))

    out = work / "out"
    dec = run([
        args.lifter, "decompile-folder", str(inp), str(out),
        "--key", "1", "--threads", "1", "--verbose", "--strict-no-synthetic-control",
    ])
    for line in (dec.stdout + dec.stderr).splitlines():
        if line.startswith("FAIL"):
            print(line)

    ok = bad = 0
    for name, src in cases:
        dst = out / f"{name}.luau"
        if not dst.exists():
            print(f"[NO OUTPUT] {name}")
            bad += 1
            continue
        text = dst.read_text(encoding="utf-8", errors="replace")
        for marker in ("controlFlowState", "GenericForInit", "GenericForNext", "NumForInit", "goto "):
            if marker in text:
                print(f"[MARKER {marker!r}] {name}")
                bad += 1
                break
        else:
            comp = run([args.compiler, "--binary", "-O0", str(dst)])
            if comp.returncode != 0:
                print(f"[RECOMPILE FAIL] {name}: {comp.stderr[:300]}")
                bad += 1
                continue
            a = run([args.luau, str(src)])
            b = run([args.luau, str(dst)])
            if a.stdout != b.stdout or a.returncode != b.returncode:
                print(f"[MISMATCH] {name}\n--- original\n{a.stdout}{a.stderr[:300]}"
                      f"\n--- decompiled\n{b.stdout}{b.stderr[:300]}")
                bad += 1
            else:
                ok += 1
    print(f"semantic round-trip: ok={ok} bad={bad} total={len(cases)}")
    if not args.keep:
        shutil.rmtree(work, ignore_errors=True)
    return 0 if bad == 0 and ok == len(cases) and cases else 1


if __name__ == "__main__":
    sys.exit(main())
