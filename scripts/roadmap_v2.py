#!/usr/bin/env python3
"""Reproducible V2 source/runtime/dataflow fixtures; never rewrites baselines.

Runtime observations, instruction-tree equality and source metrics are separate
axes. A `different` symbolic tree is a review candidate, not a runtime failure.
All subprocesses have a timeout; failed and unknown cases stay in denominators.
"""
from __future__ import annotations

import argparse
import collections
import hashlib
import json
import os
import pathlib
import subprocess
import tempfile
import time

from bytecode_dataflow import compare_dataflow
from bytecode_roundtrip import compare_chunks, parse_chunk
from source_fidelity import compare_ast, conditional_count, parse_ast
from output_quality import analyze_tree


ROOT = pathlib.Path(__file__).resolve().parent.parent


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(command, *, timeout, threads=1):
    env = {k: v for k, v in os.environ.items() if not k.startswith("MEDAL_")}
    env["RAYON_NUM_THREADS"] = str(threads)
    start = time.perf_counter()
    proc = subprocess.run([str(x) for x in command], capture_output=True, timeout=timeout, env=env)
    return proc, time.perf_counter() - start


def checked(command, **kwargs):
    result, elapsed = run(command, **kwargs)
    if result.returncode != 0:
        raise RuntimeError(f"exit {result.returncode}: {result.stderr.decode(errors='replace')[:2000]}")
    return result.stdout, elapsed


def observation(command, **kwargs):
    proc, elapsed = run(command, **kwargs)
    return {"exit": proc.returncode,
            "stdout": proc.stdout.decode("utf-8", errors="strict").replace("\r\n", "\n"),
            "stderr": proc.stderr.decode("utf-8", errors="replace").replace("\r\n", "\n"),
            "seconds": elapsed}


def compiler_flags(args):
    if getattr(args, "bytecode_version", 9) == 12:
        return ["--fflags=false,LuauBytecodeCostModel=true,LuauEmitCallFeedback=true", "-t1"]
    return ["--fflags=false"]


def compile_source(args, path, opt, debug):
    raw, _ = checked([args.compiler, "--binary", f"-O{opt}", f"-g{debug}",
                      *compiler_flags(args), path], timeout=args.timeout)
    version = getattr(args, "bytecode_version", 9)
    if not raw or raw[0] != version:
        raise RuntimeError(f"expected nonempty bytecode v{version} from the pinned compiler")
    return raw


def fixture_path(root, relative):
    path = (root / relative).resolve()
    if not path.is_relative_to(root.resolve()):
        raise ValueError("fixture path escapes manifest directory")
    return path


def check_case(args, case, root, work, opt, debug):
    row = {"case": case["name"], "group": case["group"], "opt": opt, "debug": debug,
           "status": "failed", "dataflow": {"status": "unavailable"}}
    directory = work / f"{case['name']}_O{opt}_g{debug}"
    directory.mkdir()
    try:
        source = fixture_path(root, case["source"])
        driver = fixture_path(root, case["driver"]).read_text(encoding="utf-8")
        row["source_sha256"] = sha256(source)
        original = directory / "source.luau"
        original.write_bytes(source.read_bytes())
        raw = compile_source(args, original, opt, debug)
        bytecode = directory / "input.luaubc"
        bytecode.write_bytes(raw)
        command = [args.lifter, bytecode, "--strict-no-synthetic-control", *args.lifter_arg]
        output, elapsed = checked(command, timeout=args.timeout)
        row["decompile_seconds"] = elapsed
        if not output.strip():
            raise RuntimeError("empty decompiler output")
        emitted = directory / "output.luau"
        emitted.write_bytes(output)
        row["output_sha256"] = sha256(emitted)
        row["output_bytes"] = len(output)
        rebuilt = compile_source(args, emitted, opt, debug)
        row["recompile"] = "passed"
        a, b = parse_chunk(raw, 1), parse_chunk(rebuilt, 1)
        row["dataflow"] = compare_dataflow(a, b)
        pairs, missing, extra = compare_chunks(a, b)
        row["legacy_normalized"] = dict(collections.Counter(p["tier"] for p in pairs))
        row["legacy_normalized"].update(missing=len(missing), extra=len(extra))
        if args.ast:
            source_ast = parse_ast(args.ast, original, args.timeout)
            output_ast = parse_ast(args.ast, emitted, args.timeout)
            row["source_fidelity"] = compare_ast(source_ast, output_ast)
            row["output_quality"] = analyze_tree(output_ast, output.decode('utf-8'))
            row["output_conditional_expressions"] = conditional_count(output_ast)
            if row["output_conditional_expressions"]:
                raise RuntimeError("statement output style gate failed")
            if debug == 2 and "minimum_exact_names_g2" in case:
                if row["source_fidelity"].get("exact_names", -1) < case["minimum_exact_names_g2"]:
                    raise RuntimeError("binding-aware debug-name recovery gate failed")
        row["runtime"] = {}
        for variant in ("source", "output"):
            runner = directory / f"{variant}_runner.luau"
            # CALLFB emission and VM execution have separate feature flags in
            # the pinned upstream build. Disabling the VM half leaves NAMECALL
            # looking at the AUX word as an opcode and can crash the reference.
            runtime_flags = compiler_flags(args)[0]
            if getattr(args, "bytecode_version", 9) == 12:
                runtime_flags += ",LuauCallFeedback=true"
            runtime_command = ([args.luau, runtime_flags, runner]
                               if getattr(args, "bytecode_version", 9) == 12
                               else [args.luau, runner])
            if case.get("runtime_compile_inline"):
                # Compile this subject body under the actual matrix profile;
                # do not depend on require's separate module compiler settings.
                subject = (directory / f"{variant}.luau").read_text(encoding="utf-8")
                prefix = "local f = (function()\n" + subject + "\nend)()\n"
                runtime_command = [args.luau, f"-O{opt}", f"-g{debug}",
                                   runtime_flags, runner]
                row["runtime_compilation"] = "inline_body_at_matrix_profile"
            else:
                prefix = f'local f = require("./{variant}")\n'
            runner.write_text(prefix + driver,
                              encoding="utf-8", newline="\n")
            result = observation(runtime_command, timeout=args.timeout)
            row["runtime"][variant] = result
            if result["exit"] != 0 or result["stdout"] != case["expected_stdout"]:
                raise RuntimeError(f"{variant} observation differs from locked expectation")
        if args.determinism:
            repeated, _ = checked(command, timeout=args.timeout, threads=4)
            row["deterministic_threads_1_4"] = repeated == output
            if repeated != output:
                raise RuntimeError("output differs between thread counts 1 and 4")
        row["status"] = "passed"
    except (RuntimeError, ValueError, OSError, subprocess.TimeoutExpired) as error:
        row["error"] = str(error)
    return row


def check_control(args, case, work, opt):
    row = {"case": case["case"], "opt": opt, "status": "failed"}
    try:
        chunks = []
        for variant in ("a", "b"):
            path = work / f"control_{case['case']}_{variant}_O{opt}.luau"
            path.write_text(case[variant], encoding="utf-8", newline="\n")
            chunks.append(parse_chunk(compile_source(args, path, opt, 1), 1))
        row["dataflow"] = compare_dataflow(*chunks)
        row["self_control"] = compare_dataflow(chunks[0], chunks[0])
        if row["dataflow"]["status"] != "different":
            raise RuntimeError("negative control must have different use-def trees")
        if row["self_control"]["status"] != "proved":
            raise RuntimeError("positive self-control must be proved")
        row["status"] = "passed"
    except (RuntimeError, ValueError, OSError, subprocess.TimeoutExpired) as error:
        row["error"] = str(error)
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("compiler", "luau", "lifter"):
        parser.add_argument(f"--{name}", required=True, type=pathlib.Path)
    parser.add_argument("--manifest", type=pathlib.Path,
                        default=ROOT / "docs/failure_fixtures/roadmap_v2/manifest.json")
    parser.add_argument("--report", required=True, type=pathlib.Path)
    parser.add_argument("--keep", type=pathlib.Path, help="parent for a fresh work directory (never deleted)")
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--bytecode-version", type=int, choices=(9, 12), default=9,
                        help="v12 enables cost metadata, CALLFB and type info; checks input/output headers")
    parser.add_argument("--determinism", action="store_true")
    parser.add_argument("--lifter-arg", action="append", default=[], help="extra CLI flag, e.g. --lifter-arg=--synthesize-arithmetic-loops")
    parser.add_argument("--ast", type=pathlib.Path, help="pinned luau-ast executable for binding-aware metrics")
    args = parser.parse_args()
    for name in ("compiler", "luau", "lifter"):
        setattr(args, name, getattr(args, name).resolve(strict=True))
    if args.ast:
        args.ast = args.ast.resolve(strict=True)
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    if manifest["schema_version"] != 1 or not manifest["cases"]:
        parser.error("unsupported or empty manifest")
    if args.keep:
        args.keep.mkdir(parents=True, exist_ok=True)
    # A fresh child avoids stale output and never deletes caller-owned files.
    work = pathlib.Path(tempfile.mkdtemp(prefix="v2-", dir=args.keep))
    report = {"schema_version": 1, "manifest_sha256": sha256(args.manifest),
              "compiler_commit_expected": manifest["compiler_commit"],
              "tools": {name: {"path": str(getattr(args, name)), "sha256": sha256(getattr(args, name))}
                        for name in ("compiler", "luau", "lifter")},
              "compiler_flags": ["--binary", *compiler_flags(args)],
              "bytecode_version": args.bytecode_version, "lifter_args": args.lifter_arg,
              "work": str(work), "split": manifest["split"], "cases": [], "controls": []}
    if args.ast:
        report["tools"]["ast"] = {"path": str(args.ast), "sha256": sha256(args.ast)}
    for case in manifest["negative_controls"]:
        for opt in manifest["optimization_levels"]:
            report["controls"].append(check_control(args, case, work, opt))
    for case in manifest["cases"]:
        for opt in manifest["optimization_levels"]:
            for debug in manifest["debug_levels"]:
                row = check_case(args, case, args.manifest.parent, work, opt, debug)
                report["cases"].append(row)
                print(f"{row['status']}: {case['name']} O{opt} g{debug} "
                      f"dataflow={row['dataflow']['status']} {row.get('error', '')}", flush=True)
    rows = report["cases"]
    report["summary"] = {"total": len(rows),
                         "status": dict(collections.Counter(r["status"] for r in rows)),
                         "dataflow": dict(collections.Counter(r["dataflow"]["status"] for r in rows)),
                         "controls": dict(collections.Counter(r["status"] for r in report["controls"])),
                         "groups": {group: dict(collections.Counter(r["status"] for r in rows if r["group"] == group))
                                    for group in sorted({r["group"] for r in rows})}}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps(report["summary"], indent=2))
    return int(any(r["status"] != "passed" for r in [*rows, *report["controls"]]))


if __name__ == "__main__":
    raise SystemExit(main())
