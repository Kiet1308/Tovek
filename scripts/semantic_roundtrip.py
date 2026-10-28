#!/usr/bin/env python3
"""Semantic round-trip check for the generic/numeric `for` structuring proofs.

For every `*.luau` under the fixture directory:

1. compile it with the official Luau compiler at -O0, -O1 and -O2 (bytecode
   version 9, the version Roblox ships, via `--fflags=false`);
2. decompile the bytecode with the lifter in strict (fail-closed) mode;
3. compile the emitted source at O0 for syntax and at the input optimization
   for execution, with the same bytecode version and debug flags;
4. execute the actual input and rebuilt bytecode with benchmark-vm and compare
   exit status, stdout and stderr without normalization. The separate deep-review
   matrix covers all nine input/output optimization combinations.

A companion prelude/<source filename> initializes external globals in a separate
chunk, before either input executes. Preludes use fixed O1/g1 and are not inputs
to the decompiler; their identities are recorded with each runtime pair.

Missing output fails unless its exact source/profile has a reviewed typed refusal;
refusals are reported separately from runtime passes. Recompile/runtime failures
are never covered by that allowance.

Usage:
    semantic_roundtrip.py --compiler PATH --vm PATH --lifter PATH [--fixtures DIR]
"""
import argparse
import base64
import collections
import hashlib
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile

from expected_refusals import batch_refusals_match, refusal_evidence


def run(cmd, **kw):
    return subprocess.run(
        cmd, capture_output=True, text=True, encoding="utf-8", errors="replace", **kw
    )


def compile_bytecode(compiler, source, optimization):
    return subprocess.run(
        [compiler, "--binary", f"-O{optimization}", "-g1", "--fflags=false", str(source)],
        capture_output=True, timeout=60,
    )


def observe(vm, bytecode, driver):
    result = subprocess.run([vm, str(bytecode), str(driver)], capture_output=True, timeout=8)
    return dict(exit_code=result.returncode,
                stdout=result.stdout.decode("utf-8", errors="surrogateescape"),
                stderr=result.stderr.decode("utf-8", errors="surrogateescape"))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--compiler", required=True, help="luau-compile executable")
    parser.add_argument("--vm", required=True, help="benchmark-vm bytecode runner")
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
    for name in ("compiler", "vm", "lifter"):
        setattr(args, name, str(pathlib.Path(getattr(args, name)).resolve()))

    fixtures = pathlib.Path(args.fixtures)
    work = pathlib.Path(args.keep) if args.keep else pathlib.Path(tempfile.mkdtemp(prefix="tovek_rt_"))
    if work.exists():
        shutil.rmtree(work)
    inp = work / "in"
    inp.mkdir(parents=True)
    raw_inputs = work / "bytecode"
    raw_inputs.mkdir()
    driver_source = work / "driver.luau"
    driver_source.write_text("", encoding="utf-8")
    driver = work / "driver.luaubc"
    compiled_driver = compile_bytecode(args.compiler, driver_source, 0)
    if compiled_driver.returncode != 0 or not compiled_driver.stdout or compiled_driver.stdout[0] != 9:
        print(f"[DRIVER COMPILE FAIL] {compiled_driver.stderr.decode(errors='replace')[:300]}")
        return 1
    driver.write_bytes(compiled_driver.stdout)
    rebuilt = work / "rebuilt"
    rebuilt.mkdir()
    refusal_manifest = fixtures / "expected_refusals.json"
    reviewed = {}
    if refusal_manifest.exists():
        manifest = json.loads(refusal_manifest.read_text(encoding="utf-8"))
        if manifest["schema_version"] != 1:
            parser.error("unsupported refusal manifest")
        reviewed = {entry["source"]: entry for entry in manifest["cases"]}
        if len(reviewed) != len(manifest["cases"]):
            parser.error("duplicate refusal source")

    cases, preludes = [], {}
    for src in sorted(fixtures.glob("*.luau")):
        prelude = fixtures / "prelude" / src.name
        if prelude.is_file():
            data = compile_bytecode(args.compiler, prelude, 1)
            if data.returncode != 0 or not data.stdout or data.stdout[0] != 9:
                print(f"[PRELUDE COMPILE FAIL] {src.name}: {data.stderr.decode(errors='replace')[:300]}")
                return 1
            prelude_bytecode = work / "prelude" / src.with_suffix(".luaubc").name
            prelude_bytecode.parent.mkdir(exist_ok=True)
            prelude_bytecode.write_bytes(data.stdout)
            preludes[src.name] = dict(source=str(prelude.resolve()),
                                     source_sha256=hashlib.sha256(prelude.read_bytes()).hexdigest(),
                                     bytecode=str(prelude_bytecode.resolve()),
                                     bytecode_sha256=hashlib.sha256(data.stdout).hexdigest(),
                                     optimization=1, debug=1, compiler_flags=["--fflags=false"])
        for opt in ("0", "1", "2"):
            name = f"{src.stem}__O{opt}"
            raw = compile_bytecode(args.compiler, src, opt)
            if raw.returncode != 0 or not raw.stdout:
                print(f"[COMPILE FAIL] {name}: {raw.stderr.decode(errors='replace')[:300]}")
                return 1
            if raw.stdout[0] != 9:
                print(f"[UNEXPECTED BYTECODE VERSION] {name}: {raw.stdout[0]}")
                return 1
            (inp / f"{name}.lua").write_text(base64.b64encode(raw.stdout).decode())
            bytecode = raw_inputs / f"{name}.luaubc"
            bytecode.write_bytes(raw.stdout)
            cases.append((name, src, int(opt), bytecode))

    out = work / "out"
    dec = run([
        args.lifter, "decompile-folder", str(inp), str(out),
        "--key", "1", "--threads", "1", "--verbose", "--strict-no-synthetic-control",
    ])
    for line in (dec.stdout + dec.stderr).splitlines():
        if line.startswith("FAIL"):
            print(line)

    ok = bad = refused = 0
    rows = []
    for name, src, opt, bytecode in cases:
        row = dict(case=name, source=str(src.resolve()),
                   source_sha256=hashlib.sha256(src.read_bytes()).hexdigest(),
                   opt=opt, output_optimization=opt, debug=1,
                   input=str(bytecode.resolve()), status="failed")
        prelude = preludes.get(src.name)
        row["runtime_order"] = "prelude_then_input" if prelude else "input_then_empty_driver"
        if prelude:
            row["prelude"] = prelude
        rows.append(row)
        dst = out / f"{name}.luau"
        if dst.is_symlink() or (dst.exists() and dst.stat().st_size == 0):
            row["error"] = "empty or linked output artifact"
            print(f"[INVALID OUTPUT] {name}")
            bad += 1
            continue
        if not dst.exists():
            single = run([args.lifter, str(bytecode), "--strict-no-synthetic-control"])
            outcome = dict(exit_code=single.returncode, stdout=single.stdout, stderr=single.stderr)
            profile = dict(opt=opt, debug=1, bytecode_version=9,
                           compiler_flags=["--fflags=false"], lifter_args=[])
            refusal = refusal_evidence(reviewed.get(src.name, {}), row["source_sha256"], profile, outcome)
            if refusal:
                row.update(status="expected_refusal", refusal=refusal, output_bytes=0)
                refused += 1
                print(f"[EXPECTED REFUSAL] {name}: {refusal['reason']}")
                continue
            row["decompile"] = outcome
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
            comp = compile_bytecode(args.compiler, dst, 0)
            if comp.returncode == 0 and opt != 0:
                comp = compile_bytecode(args.compiler, dst, opt)
            if comp.returncode != 0 or not comp.stdout or comp.stdout[0] != 9:
                print(f"[RECOMPILE FAIL] {name}: {comp.stderr.decode(errors='replace')[:300]}")
                bad += 1
                continue
            output_bytecode = rebuilt / f"{name}.luaubc"
            output_bytecode.write_bytes(comp.stdout)
            try:
                a = observe(args.vm, prelude["bytecode"], bytecode) if prelude else observe(args.vm, bytecode, driver)
                b = observe(args.vm, prelude["bytecode"], output_bytecode) if prelude else observe(args.vm, output_bytecode, driver)
            except subprocess.TimeoutExpired as error:
                row["error"] = str(error)
                print(f"[RUNTIME TIMEOUT] {name}")
                bad += 1
                continue
            row.update(input_observation=a, rebuilt_observation=b,
                       rebuilt_bytecode=str(output_bytecode.resolve()))
            if a["exit_code"] != 0 or a != b:
                print(f"[MISMATCH] {name}\n--- original\n{a!r}\n--- decompiled\n{b!r}")
                bad += 1
            else:
                ok += 1
                row.update(status="passed", output=str(dst.resolve()),
                           output_sha256=hashlib.sha256(dst.read_bytes()).hexdigest(),
                           output_bytes=dst.stat().st_size)
    batch = dict(exit_code=dec.returncode, stdout=dec.stdout, stderr=dec.stderr)
    expected_failures = {f"{row['case']}.lua": row["refusal"]["stderr"]
                         for row in rows if row["status"] == "expected_refusal"}
    batch_valid = batch_refusals_match(batch, expected_failures,
                                      {f"{name}.lua" for name, *_ in cases}, str(out.resolve()))
    if not batch_valid:
        print("[BATCH DIAGNOSTICS MISMATCH] output inventory or diagnostic text differs")
        bad += 1
    report = dict(schema_version=1, fixtures_path=str(fixtures.resolve()),
                  manifest_path=str(refusal_manifest.resolve()),
                  manifest_sha256=hashlib.sha256(refusal_manifest.read_bytes()).hexdigest() if refusal_manifest.exists() else None,
                  bytecode_version=9, compiler_flags=["--fflags=false"], lifter_args=[],
                  empty_driver=dict(source_sha256=hashlib.sha256(driver_source.read_bytes()).hexdigest(),
                                    bytecode_sha256=hashlib.sha256(driver.read_bytes()).hexdigest(),
                                    optimization=0, debug=1, compiler_flags=["--fflags=false"]),
                  tools={name: dict(path=getattr(args, name), sha256=hashlib.sha256(pathlib.Path(getattr(args, name)).read_bytes()).hexdigest())
                         for name in ("compiler", "vm", "lifter")},
                  summary=dict(total=len(cases), status=dict(collections.Counter(row["status"] for row in rows)),
                               batch_exit=dec.returncode, batch_valid=batch_valid), batch=batch, cases=rows)
    (work / "results.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"semantic round-trip: ok={ok} expected_refusal={refused} bad={bad} total={len(cases)}")
    if not args.keep:
        shutil.rmtree(work, ignore_errors=True)
    return 0 if bad == 0 and ok + refused == len(cases) and cases else 1


if __name__ == "__main__":
    sys.exit(main())
